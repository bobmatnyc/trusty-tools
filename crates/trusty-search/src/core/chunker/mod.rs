//! AST-aware code chunker built on tree-sitter.
//!
//! Why: a sliding-window chunker fragments declarations and produces noisy
//! BM25/vector candidates because a single function may straddle two windows.
//! AST-aware chunking yields one chunk per top-level declaration, making
//! `function_name`, `chunk_type`, and `calls` accurate enough to drive both
//! semantic search and the knowledge-graph CALLS edges (#5, #17).
//!
//! What: `chunk_ast(file, content, language) -> (Vec<RawChunk>, Vec<RawEntity>)`
//! parses with tree-sitter, walks top-level declarations into chunks, populates
//! per-chunk fields (calls, inherits_from, nlp_keywords, …), splits oversized
//! chunks into sub-chunks with stable parent IDs, and emits a flat entity list
//! in the same pass. Unknown extensions fall back to `chunk_text()`.
//!
//! Test: see `#[cfg(test)]` below — covers function/method chunking, qualified
//! method names, calls extraction, named-type entities, large-function
//! splitting, unknown-language fallback, and doc-comment NLP keywords.

mod classify;
mod inherits;
mod walk;

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Parser, Tree};

use crate::core::entity::{extract_entities, RawEntity};

use self::walk::{build_line_offsets, split_oversized, walk_for_chunks};

/// Per-thread cached tree-sitter parsers, one per supported language.
///
/// Why: `chunk_ast` is called once per file from a rayon `par_iter` batch
/// (typically 128 files at a time). Tree-sitter `Parser` is not `Send`, so we
/// cannot share a single parser across worker threads — but a fresh parser
/// requires a C-FFI grammar load on every call, which is wasted work when the
/// same worker thread processes many files of the same language. Caching one
/// `RefCell<Parser>` per language in thread-local storage amortises the
/// grammar load to once-per-worker-per-language. `RefCell` is required because
/// `Parser::parse()` takes `&mut self`; this is safe because each rayon worker
/// owns its own thread-local copy and there is no cross-thread aliasing.
///
/// What: one `thread_local!` slot per language supported by `language_for`.
/// Each slot lazily constructs a `Parser` with the language set on first use
/// in that thread. `with_parser(lang_tag, f)` borrows the matching parser
/// mutably and invokes `f` with it; returns `None` if `lang_tag` is unknown.
///
/// Test: existing chunker tests (`test_rust_function_chunking`,
/// `test_csharp_chunking`, `test_kotlin_chunking`, `test_swift_chunking`,
/// `test_scala_*`, `test_php_*`, etc.) all exercise `chunk_ast` for the
/// covered languages and must continue to pass — the cache must not change
/// chunk output, only how parsers are constructed.
macro_rules! ts_parser_thread_locals {
    ($($name:ident => $lang_expr:expr),* $(,)?) => {
        $(
            thread_local! {
                static $name: RefCell<Parser> = RefCell::new({
                    let mut p = Parser::new();
                    let lang: Language = $lang_expr.into();
                    p.set_language(&lang).expect("tree-sitter grammar load");
                    p
                });
            }
        )*
    };
}

ts_parser_thread_locals! {
    PARSER_RUST       => tree_sitter_rust::LANGUAGE,
    PARSER_PYTHON     => tree_sitter_python::LANGUAGE,
    PARSER_JAVASCRIPT => tree_sitter_javascript::LANGUAGE,
    PARSER_TYPESCRIPT => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
    PARSER_TSX        => tree_sitter_typescript::LANGUAGE_TSX,
    PARSER_GO         => tree_sitter_go::LANGUAGE,
    PARSER_JAVA       => tree_sitter_java::LANGUAGE,
    PARSER_C          => tree_sitter_c::LANGUAGE,
    PARSER_CPP        => tree_sitter_cpp::LANGUAGE,
    PARSER_RUBY       => tree_sitter_ruby::LANGUAGE,
    PARSER_PHP        => tree_sitter_php::LANGUAGE_PHP,
    PARSER_SCALA      => tree_sitter_scala::LANGUAGE,
    PARSER_CSHARP     => tree_sitter_c_sharp::LANGUAGE,
    PARSER_KOTLIN     => tree_sitter_kotlin_ng::LANGUAGE,
    PARSER_SWIFT      => tree_sitter_swift::LANGUAGE,
}

/// Identifies which cached parser to use. Distinct from the human-facing
/// `language` tag because `.ts` and `.tsx` share the `"typescript"` tag but
/// require different parsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserKind {
    Rust,
    Python,
    Javascript,
    Typescript,
    Tsx,
    Go,
    Java,
    C,
    Cpp,
    Ruby,
    Php,
    Scala,
    Csharp,
    Kotlin,
    Swift,
}

/// Borrow this thread's cached parser for `kind` and parse `src`.
///
/// Why: callers want a `Tree`, not a `Parser`; the closure form keeps the
/// `RefCell` borrow scoped tightly so it cannot leak past the parse call.
/// What: dispatches to the matching thread-local parser and returns whatever
/// `Parser::parse` produces (`None` on malformed source).
/// Test: covered transitively via every chunker integration test that exercises
/// `chunk_ast` for the listed languages.
fn parse_with_cached(kind: ParserKind, src: &[u8]) -> Option<Tree> {
    match kind {
        ParserKind::Rust => PARSER_RUST.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Python => PARSER_PYTHON.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Javascript => PARSER_JAVASCRIPT.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Typescript => PARSER_TYPESCRIPT.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Tsx => PARSER_TSX.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Go => PARSER_GO.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Java => PARSER_JAVA.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::C => PARSER_C.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Cpp => PARSER_CPP.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Ruby => PARSER_RUBY.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Php => PARSER_PHP.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Scala => PARSER_SCALA.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Csharp => PARSER_CSHARP.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Kotlin => PARSER_KOTLIN.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Swift => PARSER_SWIFT.with(|p| p.borrow_mut().parse(src, None)),
    }
}

///
/// `Default` is `Unknown` so chunks deserialized from older index versions
/// (which lacked `chunk_type`) round-trip cleanly.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChunkType {
    #[default]
    Unknown,
    Function,
    Method,
    Class,
    Struct,
    Impl,
    Module,
    Trait,
    Enum,
    Test,
    Constant,
    TypeAlias,
    Docstring,
    /// Free-form code that doesn't fit a more specific category.
    FreeCode,
    /// Legacy alias for `FreeCode` — retained for backwards-compatible deserialization.
    Code,
}

impl ChunkType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Function => "Function",
            Self::Method => "Method",
            Self::Class => "Class",
            Self::Struct => "Struct",
            Self::Impl => "Impl",
            Self::Module => "Module",
            Self::Trait => "Trait",
            Self::Enum => "Enum",
            Self::Test => "Test",
            Self::Constant => "Constant",
            Self::TypeAlias => "TypeAlias",
            Self::Docstring => "Docstring",
            Self::FreeCode => "FreeCode",
            Self::Code => "Code",
        }
    }
}

/// A code chunk extracted from a source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawChunk {
    pub id: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub function_name: Option<String>,
    pub language: Option<String>,

    // Issue #4 / #17 additions
    pub chunk_type: ChunkType,
    pub calls: Vec<String>,
    pub inherits_from: Vec<String>,
    pub chunk_depth: usize,
    pub parent_chunk_id: Option<String>,
    pub child_chunk_ids: Vec<String>,
    pub nlp_keywords: Vec<String>,
    pub nlp_code_refs: Vec<String>,

    /// Entity-derived virtual terms appended to this chunk's BM25 document
    /// at index time (issue #19). Not displayed to users; used only to give
    /// BM25 extra surface area to match symbolic queries against.
    #[serde(default)]
    pub virtual_terms: Vec<String>,
}
impl RawChunk {
    /// Build a generic `Code` chunk — used by `chunk_text` and the unknown-extension fallback.
    fn generic(
        id: String,
        file: String,
        start_line: usize,
        end_line: usize,
        content: String,
    ) -> Self {
        Self {
            id,
            file,
            start_line,
            end_line,
            content,
            function_name: None,
            language: None,
            chunk_type: ChunkType::Code,
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
}

/// Overlapping sliding-window chunker. Retained for unknown extensions and as
/// the backing routine for sub-chunking oversized AST chunks.
pub fn chunk_text(file: &str, content: &str, window: usize, stride: usize) -> Vec<RawChunk> {
    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < lines.len() {
        let end = (start + window).min(lines.len());
        let text = lines[start..end].join("\n");
        chunks.push(RawChunk::generic(
            format!("{}:{}:{}", file, start + 1, end),
            file.to_string(),
            start + 1,
            end,
            text,
        ));
        if end == lines.len() {
            break;
        }
        start += stride;
    }
    chunks
}

/// Map a file extension to a (language_tag, `ParserKind`).
///
/// Why: the `language_tag` is the human-facing label embedded in chunks
/// (`"typescript"` for both `.ts` and `.tsx`), while `ParserKind` picks the
/// right thread-local parser cache slot.
/// What: pure extension → (tag, kind) lookup; returns `None` for unsupported
/// extensions so the caller can fall back to `chunk_document` / `chunk_text`.
/// Test: indirectly covered by every language-specific chunker test.
fn language_for(file: &str) -> Option<(&'static str, ParserKind)> {
    let ext = std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let pair: (&'static str, ParserKind) = match ext.as_str() {
        "rs" => ("rust", ParserKind::Rust),
        "py" => ("python", ParserKind::Python),
        "js" | "mjs" | "cjs" | "jsx" => ("javascript", ParserKind::Javascript),
        "ts" => ("typescript", ParserKind::Typescript),
        "tsx" => ("typescript", ParserKind::Tsx),
        "go" => ("go", ParserKind::Go),
        "java" => ("java", ParserKind::Java),
        "c" | "h" => ("c", ParserKind::C),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => ("cpp", ParserKind::Cpp),
        "rb" => ("ruby", ParserKind::Ruby),
        "php" => ("php", ParserKind::Php),
        "scala" => ("scala", ParserKind::Scala),
        "cs" => ("csharp", ParserKind::Csharp),
        "kt" | "kts" => ("kotlin", ParserKind::Kotlin),
        "swift" => ("swift", ParserKind::Swift),
        _ => return None,
    };
    Some(pair)
}

/// Maximum lines for a JSON file to be indexed as a single chunk. Files
/// larger than this are skipped (JSON is hard to chunk meaningfully).
const JSON_MAX_LINES: usize = 500;

/// Maximum lines per plaintext / log chunk. Long paragraphs are split.
const PLAINTEXT_MAX_LINES: usize = 50;

/// Structured document chunker.
///
/// Why: code-aware chunking is the wrong tool for prose, config, and log
/// formats; sliding-window chunking shreds heading structure. Format-aware
/// chunking yields semantically coherent BM25/vector candidates.
/// What: dispatches on extension to per-format chunkers:
///   - md/mdx  → section-per-heading
///   - yaml/yml → top-level key sections
///   - toml    → `[section]` blocks
///   - json    → whole file if < 500 lines, otherwise skip
///   - txt/log → blank-line paragraphs, capped at 50 lines/chunk
///   - xml     → top-level child elements
///
/// Returns `None` for unknown extensions so the caller can fall back to the
/// sliding-window chunker.
///
/// Test: see `test_chunk_markdown_*`, `test_chunk_yaml_*`, etc. below.
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
fn document_chunk(
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
fn chunk_markdown(file: &str, content: &str) -> Vec<RawChunk> {
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
fn chunk_yaml(file: &str, content: &str) -> Vec<RawChunk> {
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
fn chunk_toml(file: &str, content: &str) -> Vec<RawChunk> {
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

/// Generic top-level-key chunker. `header_of(line)` returns `Some(name)` when
/// the line starts a new section. Content before the first header is emitted
/// as a leading "preamble" chunk.
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
fn chunk_json(file: &str, content: &str) -> Option<Vec<RawChunk>> {
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
fn chunk_plaintext(file: &str, content: &str) -> Vec<RawChunk> {
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
fn chunk_xml(file: &str, content: &str) -> Vec<RawChunk> {
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
fn count_xml_closes(line: &str) -> usize {
    line.matches("</").count()
}

/// Extract the first opening tag name from a line, e.g. `<book id="1">` → `book`.
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

/// AST-aware entry point. Returns chunks and entities produced from a single
/// parse pass. For structured documents (md, yaml, toml, json, xml, txt, log)
/// dispatches to `chunk_document`. Falls back to `chunk_text` for unknown
/// extensions.
pub fn chunk_ast(file: &str, content: &str) -> (Vec<RawChunk>, Vec<RawEntity>) {
    let Some((lang, kind)) = language_for(file) else {
        // Try structured-document chunkers (markdown, yaml, toml, json, xml,
        // plaintext, logs). These return None for unknown extensions and we
        // fall back to the sliding-window chunker.
        if let Some(chunks) = chunk_document(file, content) {
            return (chunks, Vec::new());
        }
        return (chunk_text(file, content, 150, 50), Vec::new());
    };

    let src = content.as_bytes();
    let Some(tree) = parse_with_cached(kind, src) else {
        return (chunk_text(file, content, 150, 50), Vec::new());
    };

    let line_offsets = build_line_offsets(src);
    let mut chunks: Vec<RawChunk> = Vec::new();
    walk_for_chunks(
        tree.root_node(),
        src,
        file,
        lang,
        &line_offsets,
        0,
        &mut chunks,
    );

    if chunks.is_empty() {
        // Source had no recognisable declarations: fall back to a single Code chunk.
        let total_lines = content.lines().count().max(1);
        chunks.push(RawChunk::generic(
            format!("{file}:1:{total_lines}"),
            file.to_string(),
            1,
            total_lines,
            content.to_string(),
        ));
        if let Some(c) = chunks.first_mut() {
            c.language = Some(lang.to_string());
        }
    }

    // Split oversized chunks; produces sub-chunks with `parent_chunk_id`.
    let split = split_oversized(chunks);

    // Entities (single pass over the same tree).
    let entities = extract_entities(&tree, src, file, lang);

    (split, entities)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overlapping_chunks() {
        let content = (1..=200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_text("test.txt", &content, 150, 50);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[1].start_line, 51);
    }

    #[test]
    fn test_chunk_id_format() {
        let chunks = chunk_text("src/main.txt", "line1\nline2\nline3", 150, 50);
        assert!(chunks[0].id.starts_with("src/main.txt:"));
    }

    #[test]
    fn test_rust_function_chunking() {
        let src = r#"
fn alpha() {}

fn beta() -> i32 { 1 }

fn gamma(x: i32) -> i32 { x + 1 }
"#;
        let (chunks, _ents) = chunk_ast("a.rs", src);
        let fns: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Function)
            .collect();
        assert_eq!(fns.len(), 3, "expected 3 function chunks, got {fns:?}");
        let names: Vec<_> = fns
            .iter()
            .map(|c| c.function_name.clone().unwrap_or_default())
            .collect();
        assert!(names.contains(&"alpha".to_string()));
        assert!(names.contains(&"beta".to_string()));
        assert!(names.contains(&"gamma".to_string()));
    }

    #[test]
    fn test_rust_impl_method_qualified_name() {
        let src = r#"
struct Foo;
impl Foo {
    fn bar(&self) {}
}
"#;
        let (chunks, _) = chunk_ast("foo.rs", src);
        let method = chunks
            .iter()
            .find(|c| c.chunk_type == ChunkType::Method)
            .expect("expected at least one Method chunk");
        assert_eq!(method.function_name.as_deref(), Some("Foo::bar"));
    }

    #[test]
    fn test_rust_calls_extraction() {
        let src = r#"
fn main() {
    foo();
    bar(1, 2);
}
fn foo() {}
fn bar(_a: i32, _b: i32) {}
"#;
        let (chunks, _) = chunk_ast("m.rs", src);
        let main_chunk = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("main"))
            .expect("main chunk");
        assert!(
            main_chunk.calls.contains(&"foo".to_string()),
            "calls={:?}",
            main_chunk.calls
        );
        assert!(
            main_chunk.calls.contains(&"bar".to_string()),
            "calls={:?}",
            main_chunk.calls
        );
    }

    #[test]
    fn test_rust_entity_named_types() {
        let src = r#"
use std::sync::Arc;
fn f() {
    let _x: Arc<Vec<String>> = Arc::new(Vec::new());
}
"#;
        let (_chunks, entities) = chunk_ast("t.rs", src);
        let named: Vec<&str> = entities
            .iter()
            .filter(|e| e.entity_type == crate::core::entity::EntityType::NamedType)
            .map(|e| e.text.as_str())
            .collect();
        assert!(named.contains(&"Arc"), "named_types={named:?}");
        assert!(named.contains(&"Vec"), "named_types={named:?}");
        assert!(named.contains(&"String"), "named_types={named:?}");
    }

    #[test]
    fn test_large_function_splits() {
        // 250-line function body
        let mut body = String::new();
        for i in 0..250 {
            body.push_str(&format!("    let _v{i} = {i};\n"));
        }
        let src = format!("fn huge() {{\n{body}}}\n");
        let (chunks, _) = chunk_ast("h.rs", &src);
        let subs: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.parent_chunk_id.is_some())
            .collect();
        assert!(
            !subs.is_empty(),
            "expected sub-chunks for 250-line fn, got {chunks:#?}"
        );
        let parent_id = subs[0].parent_chunk_id.clone().unwrap();
        let parent = chunks
            .iter()
            .find(|c| c.id == parent_id)
            .expect("parent retained");
        assert!(!parent.child_chunk_ids.is_empty());
    }

    #[test]
    fn test_unknown_language_fallback() {
        // Use an unknown extension (no document chunker matches) to verify the
        // sliding-window fallback path.
        let content = "hello world\nfoo bar\nbaz";
        let (chunks, entities) = chunk_ast("notes.unknownext", content);
        assert!(entities.is_empty());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_type, ChunkType::Code);
    }

    #[test]
    fn test_chunk_markdown_sections() {
        let content = "# Title\n\nintro\n\n## Section A\n\nbody a\n\n## Section B\n\nbody b\n";
        let chunks = chunk_markdown("doc.md", content);
        assert!(
            chunks.len() >= 2,
            "expected multiple sections, got {chunks:#?}"
        );
        let names: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.function_name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "Section A"), "names={names:?}");
        assert!(names.iter().any(|n| n == "Section B"), "names={names:?}");
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("markdown"));
            assert_eq!(c.chunk_type, ChunkType::Docstring);
        }
    }

    #[test]
    fn test_chunk_markdown_ignores_hash_in_code_fence() {
        let content = "# Real Heading\n\nintro\n\n```\n## not a heading\n```\n\n## Next\n\nx\n";
        let chunks = chunk_markdown("doc.md", content);
        let names: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.function_name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "Real Heading"));
        assert!(names.iter().any(|n| n == "Next"));
        assert!(
            !names.iter().any(|n| n == "not a heading"),
            "should not split on # inside fenced code block: {names:?}"
        );
    }

    #[test]
    fn test_chunk_yaml_top_level_keys() {
        let content = "name: foo\nversion: 1.0\n\ndeps:\n  - a\n  - b\n\nscripts:\n  build: x\n";
        let chunks = chunk_yaml("conf.yaml", content);
        let names: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.function_name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "name"), "names={names:?}");
        assert!(names.iter().any(|n| n == "deps"), "names={names:?}");
        assert!(names.iter().any(|n| n == "scripts"), "names={names:?}");
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("yaml"));
        }
    }

    #[test]
    fn test_chunk_toml_sections() {
        let content = "[package]\nname = \"foo\"\nversion = \"1.0\"\n\n[dependencies]\nserde = \"1\"\n\n[[bin]]\nname = \"x\"\n";
        let chunks = chunk_toml("Cargo.toml", content);
        let names: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.function_name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "package"), "names={names:?}");
        assert!(names.iter().any(|n| n == "dependencies"), "names={names:?}");
        assert!(names.iter().any(|n| n == "bin"), "names={names:?}");
    }

    #[test]
    fn test_chunk_json_small_file_single_chunk() {
        let content = "{\n  \"name\": \"foo\",\n  \"version\": \"1.0\"\n}\n";
        let chunks = chunk_json("a.json", content).expect("Some result");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].language.as_deref(), Some("json"));
    }

    #[test]
    fn test_chunk_json_large_file_skipped() {
        let big = (0..600)
            .map(|i| format!("  \"k{i}\": {i},"))
            .collect::<Vec<_>>()
            .join("\n");
        let content = format!("{{\n{big}\n}}\n");
        let chunks = chunk_json("big.json", &content).expect("Some result");
        assert!(chunks.is_empty(), "expected large JSON to be skipped");
    }

    #[test]
    fn test_chunk_plaintext_paragraphs() {
        let content = "First paragraph line 1.\nFirst paragraph line 2.\n\nSecond paragraph line 1.\nSecond paragraph line 2.\n\nThird paragraph.\n";
        let chunks = chunk_plaintext("note.txt", content);
        assert_eq!(
            chunks.len(),
            3,
            "expected one chunk per paragraph, got {chunks:#?}"
        );
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("text"));
        }
    }

    #[test]
    fn test_chunk_plaintext_caps_at_50_lines() {
        let content = (1..=130)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_plaintext("big.log", &content);
        assert!(
            chunks.len() >= 3,
            "expected at least 3 chunks for 130-line paragraph, got {}",
            chunks.len()
        );
        for c in &chunks {
            let line_count = c.end_line.saturating_sub(c.start_line) + 1;
            assert!(line_count <= 50, "chunk too large: {line_count} lines");
            assert_eq!(c.language.as_deref(), Some("log"));
        }
    }

    #[test]
    fn test_chunk_xml_top_level_children() {
        let content = "<?xml version=\"1.0\"?>\n<library>\n  <book id=\"1\">\n    <title>A</title>\n  </book>\n  <book id=\"2\">\n    <title>B</title>\n  </book>\n  <magazine>\n    <title>C</title>\n  </magazine>\n</library>\n";
        let chunks = chunk_xml("data.xml", content);
        let names: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.function_name.clone())
            .collect();
        assert!(
            names.iter().filter(|n| *n == "book").count() >= 2,
            "names={names:?}"
        );
        assert!(names.iter().any(|n| n == "magazine"), "names={names:?}");
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("xml"));
        }
    }

    #[test]
    fn test_chunk_document_dispatch() {
        // Verify chunk_ast routes structured documents through chunk_document.
        let md_content = "# Hello\n\nworld\n";
        let (md_chunks, _) = chunk_ast("readme.md", md_content);
        assert!(md_chunks
            .iter()
            .any(|c| c.language.as_deref() == Some("markdown")));

        let yaml_content = "key: value\n";
        let (yaml_chunks, _) = chunk_ast("conf.yml", yaml_content);
        assert!(yaml_chunks
            .iter()
            .any(|c| c.language.as_deref() == Some("yaml")));

        let toml_content = "[section]\nx = 1\n";
        let (toml_chunks, _) = chunk_ast("a.toml", toml_content);
        assert!(toml_chunks
            .iter()
            .any(|c| c.language.as_deref() == Some("toml")));
    }

    #[test]
    fn test_nlp_code_refs() {
        let src = r#"
/// Wraps the `CodeIndexer` to expose hybrid search.
fn make() {}
"#;
        let (chunks, _) = chunk_ast("d.rs", src);
        let f = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("make"))
            .unwrap();
        assert!(
            f.nlp_code_refs.iter().any(|k| k == "CodeIndexer"),
            "code_refs={:?}",
            f.nlp_code_refs
        );
    }

    #[test]
    fn test_entity_external_crate() {
        let src = r#"
use usearch::Index;
fn f() {}
"#;
        let (_chunks, ents) = chunk_ast("u.rs", src);
        let exts: Vec<&str> = ents
            .iter()
            .filter(|e| e.entity_type == crate::core::entity::EntityType::ExternalCrate)
            .map(|e| e.text.as_str())
            .collect();
        assert!(exts.contains(&"usearch"), "external_crates={exts:?}");
    }

    #[test]
    fn test_entity_error_variant() {
        let src = r#"
fn f() -> Result<(), anyhow::Error> {
    anyhow::bail!("index not found");
}
"#;
        let (_chunks, ents) = chunk_ast("e.rs", src);
        let any_err = ents
            .iter()
            .any(|e| e.entity_type == crate::core::entity::EntityType::ErrorVariant);
        assert!(
            any_err,
            "expected at least one ErrorVariant entity, got {ents:#?}"
        );
    }

    #[test]
    fn test_csharp_chunking() {
        let src = r#"
namespace MyApp {
    class Foo {
        public void Bar() { Baz(); this.Qux(); }
        public Foo() {}
    }
    interface IThing { void Do(); }
}
"#;
        let (chunks, _) = chunk_ast("a.cs", src);
        // Expect: namespace (Module), class Foo (Class), Bar (Method),
        //   ctor (Method), IThing (Trait), Do (Method).
        let classes: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Class)
            .collect();
        assert!(
            classes
                .iter()
                .any(|c| c.function_name.as_deref() == Some("Foo")),
            "expected class Foo, got {chunks:#?}"
        );
        let traits: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Trait)
            .collect();
        assert!(
            traits
                .iter()
                .any(|c| c.function_name.as_deref() == Some("IThing")),
            "expected interface IThing as Trait"
        );
        let bar = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("Bar"))
            .expect("Bar method chunk");
        assert_eq!(bar.chunk_type, ChunkType::Method);
        assert!(
            bar.calls.contains(&"Baz".to_string()),
            "calls={:?}",
            bar.calls
        );
        assert!(
            bar.calls.contains(&"Qux".to_string()),
            "calls={:?}",
            bar.calls
        );
    }

    #[test]
    fn test_kotlin_chunking() {
        // Avoid the top-level `package` statement which the kotlin-ng grammar
        // parses oddly without a following file body terminator; the chunker
        // still walks into ERROR-recovered subtrees, but the cleaner case
        // exercises the happy path.
        let src = r#"
class Foo {
    fun bar() { baz(); this.qux() }
}
object Singleton {
    fun run() { other() }
}
"#;
        let (chunks, _) = chunk_ast("a.kt", src);
        assert!(
            chunks
                .iter()
                .any(|c| c.function_name.as_deref() == Some("Foo")
                    && c.chunk_type == ChunkType::Class),
            "expected class Foo, got {chunks:#?}"
        );
        let bar = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("bar"))
            .expect("bar method chunk");
        assert_eq!(bar.chunk_type, ChunkType::Method);
        assert!(
            bar.calls.contains(&"baz".to_string()),
            "calls={:?}",
            bar.calls
        );
        assert!(
            bar.calls.contains(&"qux".to_string()),
            "calls={:?}",
            bar.calls
        );
    }

    #[test]
    fn test_swift_chunking() {
        let src = r#"
class Foo {
    func bar() { baz(); self.qux() }
    init() {}
}
struct S {}
enum E { case a }
protocol P { func d() }
extension Foo { func ext() {} }
"#;
        let (chunks, _) = chunk_ast("a.swift", src);
        // class Foo
        assert!(
            chunks
                .iter()
                .any(|c| c.function_name.as_deref() == Some("Foo")
                    && c.chunk_type == ChunkType::Class),
            "expected class Foo, got {chunks:#?}"
        );
        // struct S
        assert!(
            chunks
                .iter()
                .any(|c| c.function_name.as_deref() == Some("S")
                    && c.chunk_type == ChunkType::Struct),
            "expected struct S"
        );
        // enum E
        assert!(
            chunks.iter().any(|c| c.function_name.as_deref() == Some("E")
                && c.chunk_type == ChunkType::Enum),
            "expected enum E"
        );
        // protocol P → Trait
        assert!(
            chunks.iter().any(
                |c| c.function_name.as_deref() == Some("P") && c.chunk_type == ChunkType::Trait
            ),
            "expected protocol P as Trait"
        );
        // extension Foo → Module
        assert!(
            chunks
                .iter()
                .any(|c| c.chunk_type == ChunkType::Module
                    && c.function_name.as_deref() == Some("Foo")),
            "expected extension Foo as Module"
        );
        // method calls
        let bar = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("bar"))
            .expect("bar method chunk");
        assert!(
            bar.calls.contains(&"baz".to_string()),
            "calls={:?}",
            bar.calls
        );
        assert!(
            bar.calls.contains(&"qux".to_string()),
            "calls={:?}",
            bar.calls
        );
    }

    #[test]
    fn test_nlp_keywords_from_doc_comments() {
        let src = r#"
/// Implements the RRF fusion algorithm.
fn fuse() {}
"#;
        let (chunks, _) = chunk_ast("d.rs", src);
        let f = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("fuse"))
            .unwrap();
        assert!(
            f.nlp_keywords.iter().any(|k| k == "RRF"),
            "keywords={:?}",
            f.nlp_keywords
        );
        assert!(
            f.nlp_keywords.iter().any(|k| k == "Implements"),
            "keywords={:?}",
            f.nlp_keywords
        );
    }

    // ----- Scala Phase 2 (issue #55) -----

    #[test]
    fn test_scala_method_qualified_name() {
        // Why: SymbolGraph caller edges need `ClassName::methodName` so that
        // two classes with a `run` method don't share a single graph node.
        // What: a class method is chunked as `Foo::bar`, a top-level def as `freefn`.
        // Test: assert both chunks emit the expected qualified / unqualified names.
        let src = r#"
class Foo extends Bar with Mixin {
  def bar(): Unit = baz()
}
object O {
  def run(): Unit = other()
}
def freefn(): Unit = ()
"#;
        let (chunks, _) = chunk_ast("a.scala", src);
        let bar = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("Foo::bar"))
            .expect("expected qualified method Foo::bar, got: {chunks:#?}");
        assert_eq!(bar.chunk_type, ChunkType::Method);
        let run = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("O::run"))
            .expect("expected qualified method O::run");
        assert_eq!(run.chunk_type, ChunkType::Method);
        // Top-level def remains unqualified.
        assert!(
            chunks
                .iter()
                .any(|c| c.function_name.as_deref() == Some("freefn")
                    && c.chunk_type == ChunkType::Function),
            "expected unqualified Function freefn, got {chunks:#?}"
        );
    }

    #[test]
    fn test_scala_caller_scoped_call_edges() {
        // Why: Phase 2 needs caller-scoped call edges so `who calls baz?`
        // returns `Foo::bar`, not the whole file.
        // What: `Foo::bar`'s `calls` field includes `baz`, and the call is
        // attached to the method chunk (not the class).
        // Test: assert `calls` membership on the method chunk.
        let src = r#"
class Foo {
  def bar(): Unit = {
    baz()
    this.qux()
  }
}
"#;
        let (chunks, _) = chunk_ast("a.scala", src);
        let bar = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("Foo::bar"))
            .expect("Foo::bar chunk");
        assert!(
            bar.calls.contains(&"baz".to_string()),
            "calls={:?}",
            bar.calls
        );
        assert!(
            bar.calls.contains(&"qux".to_string()),
            "calls={:?}",
            bar.calls
        );
    }

    #[test]
    fn test_scala_extends_and_with_emit_inherits() {
        // Why: `extends T1 with T2 with T3` describes a layered Scala class
        // mixin chain; Phase 2 turns each parent into an `Implements` edge so
        // intent-gated KG expansion can surface the parent.
        // What: `inherits_from` on the class chunk lists all three parents.
        // Test: assert membership.
        let src = r#"
class Foo extends Bar with Mixin with Other {
  def m(): Unit = ()
}
"#;
        let (chunks, _) = chunk_ast("a.scala", src);
        let foo = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("Foo") && c.chunk_type == ChunkType::Class)
            .expect("Foo class chunk");
        for parent in ["Bar", "Mixin", "Other"] {
            assert!(
                foo.inherits_from.iter().any(|p| p == parent),
                "expected parent {parent} in inherits_from={:?}",
                foo.inherits_from
            );
        }
    }

    #[test]
    fn test_scala_symbol_graph_resolves_caller() {
        // Why: end-to-end check that the chunker output, once fed to
        // SymbolGraph::build_from_chunks, yields a usable caller→callee edge.
        // What: build the graph from two scala chunks and assert
        // `callers_of("baz")` returns the qualified method.
        // Test: integrates chunker + symbol_graph for Phase 2.
        use crate::core::symbol_graph::SymbolGraph;
        let src = r#"
class Foo {
  def bar(): Unit = baz()
}
def baz(): Unit = ()
"#;
        let (chunks, _) = chunk_ast("s.scala", src);
        let tuples: Vec<_> = chunks
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.file.clone(),
                    c.function_name.clone(),
                    c.calls.clone(),
                    c.inherits_from.clone(),
                    c.chunk_type.clone(),
                )
            })
            .collect();
        let g = SymbolGraph::build_from_chunks(&tuples);
        let callers = g.callers_of("baz", 1);
        assert!(
            callers.iter().any(|(s, _)| s == "Foo::bar"),
            "callers={callers:?}"
        );
    }

    // ----- PHP Phase 2 (issue #49) -----

    #[test]
    fn test_php_method_qualified_name() {
        // Why: same rationale as Scala — class-qualified method names avoid
        // symbol collisions in the call graph.
        // What: `Foo::doIt` is the chunk's function_name; a free function in
        // the same file remains unqualified.
        // Test: assert both forms.
        let src = r#"<?php
class Foo extends Bar implements I1, I2 {
    public function doIt(): void {
        $this->helper();
    }
}
function freefn(): void {}
"#;
        let (chunks, _) = chunk_ast("a.php", src);
        let doit = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("Foo::doIt"))
            .expect("expected qualified Foo::doIt, got: {chunks:#?}");
        assert_eq!(doit.chunk_type, ChunkType::Method);
        assert!(
            chunks
                .iter()
                .any(|c| c.function_name.as_deref() == Some("freefn")
                    && c.chunk_type == ChunkType::Function),
            "expected unqualified Function freefn"
        );
    }

    #[test]
    fn test_php_caller_scoped_call_edges() {
        // Why: caller-scoped edges must capture all three PHP call shapes
        // (`$this->m()`, `Class::m()`, `func()`).
        // What: assert each callee appears in the method's `calls` field.
        let src = r#"<?php
class Foo {
    public function doIt(): void {
        $this->helper();
        Foo::staticCall();
        regularFunc();
    }
}
"#;
        let (chunks, _) = chunk_ast("a.php", src);
        let doit = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("Foo::doIt"))
            .expect("Foo::doIt chunk");
        for callee in ["helper", "staticCall", "regularFunc"] {
            assert!(
                doit.calls.iter().any(|c| c == callee),
                "expected callee {callee} in calls={:?}",
                doit.calls
            );
        }
    }

    #[test]
    fn test_php_implements_and_extends_emit_inherits() {
        // Why: PHP's `class Foo extends Bar implements I1, I2` carries one
        // parent class plus N interfaces; Phase 2 emits one `Implements` edge
        // for each.
        // What: assert all three names appear in `inherits_from`.
        let src = r#"<?php
class Foo extends Bar implements I1, I2 {}
"#;
        let (chunks, _) = chunk_ast("a.php", src);
        let foo = chunks
            .iter()
            .find(|c| c.function_name.as_deref() == Some("Foo") && c.chunk_type == ChunkType::Class)
            .expect("Foo class chunk");
        for parent in ["Bar", "I1", "I2"] {
            assert!(
                foo.inherits_from.iter().any(|p| p == parent),
                "expected parent {parent} in inherits_from={:?}",
                foo.inherits_from
            );
        }
    }

    #[test]
    fn test_php_interface_extends_emits_inherits() {
        // Why: PHP interfaces can extend multiple interfaces; the grammar
        // packages those parents in a `base_clause` (same shape as a class's
        // extends clause).
        // What: `interface Child extends P1, P2` → inherits_from = [P1, P2].
        let src = r#"<?php
interface Child extends P1, P2 {}
"#;
        let (chunks, _) = chunk_ast("a.php", src);
        let child = chunks
            .iter()
            .find(|c| {
                c.function_name.as_deref() == Some("Child") && c.chunk_type == ChunkType::Trait
            })
            .expect("Child interface (chunked as Trait)");
        for parent in ["P1", "P2"] {
            assert!(
                child.inherits_from.iter().any(|p| p == parent),
                "expected parent {parent} in inherits_from={:?}",
                child.inherits_from
            );
        }
    }

    #[test]
    fn test_php_symbol_graph_resolves_caller() {
        // Why: end-to-end Phase 2 integration: chunker → symbol_graph yields
        // a usable PHP caller→callee edge for KG expansion.
        // What: assert `callers_of("helper")` returns `Foo::doIt`.
        use crate::core::symbol_graph::SymbolGraph;
        let src = r#"<?php
class Foo {
    public function doIt(): void {
        $this->helper();
    }
    public function helper(): void {}
}
"#;
        let (chunks, _) = chunk_ast("p.php", src);
        let tuples: Vec<_> = chunks
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.file.clone(),
                    c.function_name.clone(),
                    c.calls.clone(),
                    c.inherits_from.clone(),
                    c.chunk_type.clone(),
                )
            })
            .collect();
        let g = SymbolGraph::build_from_chunks(&tuples);
        // `helper` resolves to `Foo::helper` via the suffix lookup.
        let callers = g.callers_of("Foo::helper", 1);
        assert!(
            callers.iter().any(|(s, _)| s == "Foo::doIt"),
            "callers={callers:?}"
        );
    }

    // ----- Per-pub-const Rust chunking (issue #143) -------------------------

    /// Why: a file containing only `pub const` declarations used to produce a
    /// single whole-file `Code` chunk with null `function_name`, making every
    /// constant invisible to symbol-name queries and the Definition-intent boost.
    /// What: each `pub const` / `pub static` in a `.rs` file must produce its own
    /// `Constant` chunk with a non-null `function_name`.
    /// Test: this is the test.
    #[test]
    fn test_rust_pub_const_chunking_produces_n_constant_chunks() {
        let src = r#"
pub const ALPHA: u32 = 1;
pub const BRUSILOV_EPOCH: u64 = 1_000_000;
pub const MAX_BATCH_SIZE: usize = 256;
pub const KIKUCHI_MAX_DEPTH: usize = 8;
pub const HNSW_EF_CONSTRUCTION: usize = 200;
pub const DEFAULT_TOP_K: usize = 10;
pub const BM25_K1: f32 = 1.5;
pub const BM25_B: f32 = 0.75;
"#;
        let (chunks, _) = chunk_ast("constants.rs", src);
        let const_chunks: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Constant)
            .collect();
        assert_eq!(
            const_chunks.len(),
            8,
            "expected 8 Constant chunks (one per pub const), got {}: {:#?}",
            const_chunks.len(),
            chunks
        );
        // Every constant chunk must have a non-null function_name.
        for c in &const_chunks {
            assert!(
                c.function_name.is_some(),
                "expected non-null function_name for constant chunk {c:#?}"
            );
        }
        // Spot-check a few names.
        let names: Vec<_> = const_chunks
            .iter()
            .filter_map(|c| c.function_name.as_deref())
            .collect();
        assert!(
            names.contains(&"BRUSILOV_EPOCH"),
            "expected BRUSILOV_EPOCH in names: {names:?}"
        );
        assert!(
            names.contains(&"MAX_BATCH_SIZE"),
            "expected MAX_BATCH_SIZE in names: {names:?}"
        );
    }

    /// Why: validates that a mixed Rust file — constants + functions — emits
    /// the correct chunk type for each item and does not absorb function chunks
    /// into constant chunks or vice-versa.
    /// What: a file with one `pub const`, one `pub fn`, and one more `pub const`
    /// must produce 3 chunks in the expected types.
    /// Test: this is the test.
    #[test]
    fn test_rust_mixed_const_and_fn_chunking() {
        let src = r#"
pub const FOO: u32 = 42;

pub fn do_something() -> u32 {
    FOO + 1
}

pub const BAR: &str = "hello";
"#;
        let (chunks, _) = chunk_ast("mixed.rs", src);

        let const_chunks: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Constant)
            .collect();
        assert_eq!(
            const_chunks.len(),
            2,
            "expected 2 Constant chunks, got {const_chunks:#?}"
        );
        let const_names: Vec<_> = const_chunks
            .iter()
            .filter_map(|c| c.function_name.as_deref())
            .collect();
        assert!(
            const_names.contains(&"FOO"),
            "expected FOO: {const_names:?}"
        );
        assert!(
            const_names.contains(&"BAR"),
            "expected BAR: {const_names:?}"
        );

        let fn_chunks: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Function)
            .collect();
        assert_eq!(
            fn_chunks.len(),
            1,
            "expected 1 Function chunk, got {fn_chunks:#?}"
        );
        assert_eq!(
            fn_chunks[0].function_name.as_deref(),
            Some("do_something"),
            "fn chunk name mismatch"
        );
    }

    /// Why: `pub static` is semantically equivalent to `pub const` for the
    /// purpose of symbol lookup; both must emit Constant chunks.
    /// What: `pub static X: &str = "…"` produces a Constant chunk with the
    /// identifier as `function_name`.
    /// Test: this is the test.
    #[test]
    fn test_rust_pub_static_treated_as_constant() {
        let src = r#"
pub static GREETING: &str = "hello";
pub static MAX_RETRIES: u32 = 3;
"#;
        let (chunks, _) = chunk_ast("statics.rs", src);
        let const_chunks: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Constant)
            .collect();
        assert_eq!(
            const_chunks.len(),
            2,
            "expected 2 Constant chunks for pub static, got {const_chunks:#?}"
        );
        let names: Vec<_> = const_chunks
            .iter()
            .filter_map(|c| c.function_name.as_deref())
            .collect();
        assert!(names.contains(&"GREETING"), "expected GREETING: {names:?}");
        assert!(
            names.contains(&"MAX_RETRIES"),
            "expected MAX_RETRIES: {names:?}"
        );
    }

    /// Why: Phase 1 scopes per-const chunking to public declarations only;
    /// private constants stay in whatever surrounding chunk applies.
    /// What: a file with private `const` (no `pub`) must NOT produce any
    /// `Constant` chunks — the private const stays unclaimed / falls through
    /// to the whole-file fallback.
    /// Test: this is the test.
    #[test]
    fn test_rust_private_const_does_not_get_constant_chunk() {
        let src = r#"
const INTERNAL_LIMIT: usize = 100;
const PRIVATE_KEY: &str = "secret";
"#;
        let (chunks, _) = chunk_ast("private.rs", src);
        let const_chunks: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Constant)
            .collect();
        assert!(
            const_chunks.is_empty(),
            "expected no Constant chunks for private consts, got {const_chunks:#?}"
        );
    }

    /// Why: regression guard — existing function/struct/impl/trait/enum
    /// chunking must not be disturbed by the new const/static rules.
    /// What: a Rust file without any `pub const` / `pub static` still produces
    /// correct Function, Struct, Impl chunks as before.
    /// Test: this is the test.
    #[test]
    fn test_rust_no_const_regression_on_function_chunks() {
        // Uses the same content as test_rust_function_chunking to ensure parity.
        let src = r#"
fn alpha() {}

fn beta() -> i32 { 1 }

fn gamma(x: i32) -> i32 { x + 1 }
"#;
        let (chunks, _) = chunk_ast("no_const.rs", src);
        let fns: Vec<&RawChunk> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Function)
            .collect();
        assert_eq!(fns.len(), 3, "expected 3 Function chunks, got {fns:#?}");
        assert!(
            chunks.iter().all(|c| c.chunk_type != ChunkType::Constant),
            "unexpected Constant chunk in function-only file: {chunks:#?}"
        );
    }

    /// Issue #90: end-to-end check that a small Rust snippet — parsed by
    /// `chunk_ast`, fed into `SymbolGraph::build_from_chunks` — produces
    /// non-zero symbols and a usable caller→callee edge. This is the
    /// regression test for the silent KG-skip bug where reindexes that
    /// breached the memory limit during embedding left the graph at 0/0.
    ///
    /// Why: prevents future skips/bugs along the chunker→graph integration
    /// from re-introducing the same failure mode.
    /// What: two free functions where `alpha` calls `beta`; the resulting
    /// graph must contain both symbols and `callers_of("beta")` must include
    /// `alpha`.
    /// Test: this is the test.
    #[test]
    fn test_rust_symbol_graph_resolves_caller() {
        use crate::core::symbol_graph::SymbolGraph;
        let src = "fn alpha() { beta(); }\nfn beta() {}\n";
        let (chunks, _) = chunk_ast("a.rs", src);
        let tuples: Vec<_> = chunks
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.file.clone(),
                    c.function_name.clone(),
                    c.calls.clone(),
                    c.inherits_from.clone(),
                    c.chunk_type.clone(),
                )
            })
            .collect();
        let g = SymbolGraph::build_from_chunks(&tuples);
        assert!(
            g.node_count() >= 2,
            "expected >= 2 symbol nodes for alpha+beta, got {} (chunks={:#?})",
            g.node_count(),
            chunks
                .iter()
                .map(|c| (c.function_name.clone(), c.calls.clone()))
                .collect::<Vec<_>>(),
        );
        let callers = g.callers_of("beta", 1);
        assert!(
            callers.iter().any(|(s, _)| s == "alpha"),
            "expected alpha among callers of beta, got {callers:?}"
        );
    }
}

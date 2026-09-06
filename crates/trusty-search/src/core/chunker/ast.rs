//! AST-aware chunking entry point: `chunk_ast`.
//!
//! Why: a sliding-window chunker fragments declarations and produces noisy
//! BM25/vector candidates because a single function may straddle two windows.
//! AST-aware chunking yields one chunk per top-level declaration, making
//! `function_name`, `chunk_type`, and `calls` accurate enough to drive both
//! semantic search and the knowledge-graph CALLS edges (#5, #17).
//!
//! What: `chunk_ast(file, content) -> (Vec<RawChunk>, Vec<RawEntity>)` parses
//! with tree-sitter via the cached per-thread parser, walks top-level
//! declarations into chunks, splits oversized chunks into sub-chunks with
//! stable parent IDs, and emits a flat entity list in the same pass. Unknown
//! extensions fall back to `chunk_document` / `chunk_text`.
//!
//! Test: all language-specific tests in `core/chunker/tests.rs` exercise this
//! entry point.

use super::document::chunk_document;
use super::parsers::{parse_with_cached, ParserKind};
use super::types::{chunk_text, RawChunk};
use crate::core::chunker::walk::{build_line_offsets, split_oversized, walk_for_chunks};
use crate::core::entity::{extract_entities, RawEntity};

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

/// AST-aware entry point. Returns chunks and entities produced from a single
/// parse pass. For structured documents (md, yaml, toml, json, xml, txt, log)
/// dispatches to `chunk_document`. Falls back to `chunk_text` for unknown
/// extensions.
///
/// Why: this is the primary indexing function; every file passes through here
/// during reindex. Correct dispatch (AST → document → sliding-window) ensures
/// the right chunker runs for every file type.
/// What: resolves the extension → language → parser, parses, walks the AST
/// into chunks, splits oversized ones, and extracts entities. Falls back to
/// `chunk_document` for structured docs and `chunk_text` for unknown types.
/// Test: `test_rust_function_chunking`, `test_chunk_document_dispatch`,
/// `test_unknown_language_fallback`, and all language-specific tests.
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

    // #6581: separate colliding ids FIRST, then split. Disambiguating before
    // the split means a sub-chunk's `{parent}::sub::{i}` id inherits an
    // already-unique parent, so neither the parent ids nor the derived sub ids
    // can collide and `parent_chunk_id` needs no remapping.
    let split = split_oversized(disambiguate_chunk_ids(file, chunks));

    // Entities (single pass over the same tree).
    let entities = extract_entities(&tree, src, file, lang);

    (split, entities)
}

/// Give every chunk in a file an id no other chunk in that file holds.
///
/// Why (#6581, superseding #6571's drop): a chunk's id is the primary key in
/// every downstream layer — the redb corpus, the BM25 document map, the HNSW key
/// map, and `IndexedFiles`. #6571 stopped the corpus corruption by keeping the
/// FIRST chunk under a colliding id and dropping the rest, which left 2,070 of
/// `app/hiring/static/assets/index.js`'s 2,299 declarations absent from search.
/// Carrying `end_line` in the id (this issue) separates most collisions, but not
/// the one that caused the incident: a minified bundle is a single physical
/// line, so every declaration in it has `start == end == 1` and two
/// `function e(…)` still land on one id.
///
/// What: keeps every chunk and appends `::dup::{n}` to the second and later
/// occurrence of any repeated id, `n` counting repeats after the first. Because
/// the id now carries `{type}::{name}::{start}::{end}`, a repeat means an
/// IDENTICAL SPAN — the scope the owner ruling of 2026-09-05 sets for the
/// suffix. The first occurrence keeps its bare id, so a corpus with no
/// collisions is byte-identical to one built without this pass. Runs BEFORE
/// `split_oversized`, where `parent_chunk_id` and `child_chunk_ids` are still
/// empty, so no reference has to be rewritten. Deterministic: the suffix follows
/// the chunker's own emission order, so the same input yields the same ids.
///
/// Test: `named_chunks_sharing_a_start_line_get_distinct_ids`,
/// `identical_span_duplicates_get_a_deterministic_dup_suffix`, and
/// `chunk_ast_never_emits_two_chunks_under_one_id` in `core/chunker/tests.rs`.
fn disambiguate_chunk_ids(file: &str, chunks: Vec<RawChunk>) -> Vec<RawChunk> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut renamed = 0usize;
    let out: Vec<RawChunk> = chunks
        .into_iter()
        .map(|mut c| {
            let count = seen.entry(c.id.clone()).or_insert(0);
            *count += 1;
            if *count > 1 {
                c.id = crate::core::chunk_id::make_dup(&c.id, *count - 1);
                renamed += 1;
            }
            c
        })
        .collect();
    if renamed > 0 {
        tracing::debug!(
            %file,
            renamed,
            kept = out.len(),
            "chunker: disambiguated chunks whose id collided within the same file",
        );
    }
    out
}

//! Tree traversal + chunk emission for the AST chunker.
//!
//! Why: `walk_for_chunks` and its helpers (`collect_calls`,
//! `preceding_doc_comments`, `nlp_from_doc`, `split_oversized`, plus the byte
//! /line-offset bookkeeping) form one cohesive responsibility: turn a parsed
//! tree-sitter tree into a flat `Vec<RawChunk>`. Lifting them into a separate
//! file keeps `chunker/mod.rs` focused on the public API and per-format
//! dispatch.
//! What: `walk_for_chunks` recursively descends the tree, emitting one chunk
//! per classified node and skipping into nested function bodies. The helpers
//! collect call sites, doc-comment NLP keywords, and split oversized chunks
//! into sub-chunks with stable parent IDs.
//! Test: covered by `test_rust_function_chunking`, `test_rust_calls_extraction`,
//! `test_large_function_splits`, `test_nlp_keywords_from_doc_comments`, and
//! many other per-language tests in `chunker/mod.rs`.

use std::collections::HashSet;

use tree_sitter::Node;

use super::classify::{
    classify_node, name_of, php_enclosing_class_name, rust_impl_type_name,
    scala_enclosing_class_name,
};
use super::inherits::collect_inherits;
use super::{ChunkType, RawChunk};

/// Maximum lines for a single AST chunk before we split into sub-chunks.
const MAX_CHUNK_LINES: usize = 200;
/// Sub-chunk window (used when splitting oversized AST chunks).
const SUB_CHUNK_WINDOW: usize = 100;
/// Sub-chunk stride.
const SUB_CHUNK_STRIDE: usize = 50;

/// Compute byte ranges → 1-based line numbers from the source bytes.
pub(super) fn line_for_byte(line_offsets: &[usize], byte: usize) -> usize {
    // line_offsets[i] = byte offset of the start of (1-based) line i+1
    match line_offsets.binary_search(&byte) {
        Ok(i) => i + 1,
        Err(i) => i.max(1),
    }
}

pub(super) fn build_line_offsets(src: &[u8]) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in src.iter().enumerate() {
        if *b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

/// Stable, content-aware chunk ID. Falls back to position when no name is available.
pub(super) fn make_chunk_id(
    file: &str,
    chunk_type: &ChunkType,
    name: &str,
    start_line: usize,
    end_line: usize,
) -> String {
    if name.is_empty() {
        format!("{file}:{start_line}:{end_line}")
    } else {
        format!("{file}::{}::{name}::{start_line}", chunk_type.as_str())
    }
}

/// Collect call expressions reachable inside `node` without descending into
/// nested function/method bodies (so a parent function doesn't claim its
/// inner-fn's calls).
///
/// Depth-capped at `MAX_WALK_DEPTH` relative to `node` (not just iterative —
/// see `walk_for_chunks` for why iteration alone isn't sufficient here):
/// `walk_for_chunks` classifies at most `MAX_WALK_DEPTH` container nodes
/// (mod/impl/class/…) per file, but each classified node triggers its own
/// call to `collect_calls`, which — uncapped — would re-walk that node's
/// *entire remaining subtree*. For a pathological chain of nested
/// containers (e.g. thousands of nested `mod` blocks) that's
/// O(MAX_WALK_DEPTH * subtree size) work, i.e. still effectively quadratic
/// in the nesting depth even though the outer walk is bounded. Capping this
/// walk's own depth keeps total per-file cost bounded by a constant
/// regardless of how deeply the input nests (issue #3537).
pub(super) fn collect_calls(node: Node<'_>, src: &[u8], lang: &str) -> Vec<String> {
    let mut out: HashSet<String> = HashSet::new();
    let mut stack: Vec<(Node, usize)> = vec![(node, 0)];
    while let Some((n, depth)) = stack.pop() {
        if depth > MAX_WALK_DEPTH {
            continue;
        }
        let kind = n.kind();
        let is_fn_kind = matches!(
            (lang, kind),
            ("rust", "function_item")
                | ("python", "function_definition")
                | ("javascript", "function_declaration")
                | ("typescript", "function_declaration")
                | ("go", "function_declaration")
                | ("java", "method_declaration")
                | ("c" | "cpp", "function_definition")
                | ("ruby", "method")
                | ("ruby", "singleton_method")
                | ("php", "function_definition")
                | ("php", "method_declaration")
                | ("scala", "function_definition")
                | ("csharp", "method_declaration")
                | ("csharp", "constructor_declaration")
                | ("kotlin", "function_declaration")
                | ("kotlin", "secondary_constructor")
                | ("swift", "function_declaration")
                | ("swift", "init_declaration")
                | ("swift", "protocol_function_declaration")
        );

        // Don't descend into nested function bodies (we treat them as their own chunks).
        if is_fn_kind && n.id() != node.id() {
            continue;
        }

        // Tree-sitter call node names per language.
        let is_call = matches!(
            (lang, kind),
            ("rust", "call_expression")
                | ("python", "call")
                | ("javascript" | "typescript", "call_expression")
                | ("go", "call_expression")
                | ("java", "method_invocation")
                | ("c" | "cpp", "call_expression")
                | ("ruby", "call")
                | ("php", "function_call_expression")
                | ("php", "member_call_expression")
                | ("php", "scoped_call_expression")
                | ("php", "nullsafe_member_call_expression")
                | ("scala", "call_expression")
                | ("csharp", "invocation_expression")
                | ("kotlin", "call_expression")
                | ("swift", "call_expression")
        );

        if is_call {
            // function/method name field varies; try common ones.
            let callee = n
                .child_by_field_name("function")
                .or_else(|| n.child_by_field_name("name"))
                .or_else(|| n.child(0));
            if let Some(c) = callee {
                let raw = std::str::from_utf8(&src[c.start_byte()..c.end_byte()])
                    .unwrap_or("")
                    .to_string();
                // Reduce `foo::bar::baz` and `obj.method` to the last identifier
                // segment for simple-name matching, but also keep the full path
                // so KG resolution can prefer qualified matches.
                let simple = raw
                    .rsplit(['.', ':'])
                    .next()
                    .unwrap_or(&raw)
                    .trim()
                    .to_string();
                if !simple.is_empty() {
                    out.insert(simple);
                }
            }
        }

        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push((child, depth + 1));
        }
    }
    let mut v: Vec<String> = out.into_iter().collect();
    v.sort();
    v
}

/// Pull doc-comment text immediately preceding `node` (Rust `///` and `//!`).
pub(super) fn preceding_doc_comments(node: Node<'_>, src: &[u8]) -> String {
    let mut buf = String::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "line_comment" || p.kind() == "block_comment" {
            let txt = std::str::from_utf8(&src[p.start_byte()..p.end_byte()]).unwrap_or("");
            if txt.starts_with("///") || txt.starts_with("//!") || txt.starts_with("/**") {
                buf.insert_str(0, txt);
                buf.insert(0, '\n');
            }
            prev = p.prev_sibling();
        } else {
            break;
        }
    }
    buf
}

/// Cheap noun-phrase-ish keyword extraction from doc comments.
pub(super) fn nlp_from_doc(doc: &str) -> (Vec<String>, Vec<String>) {
    let mut keywords: Vec<String> = Vec::new();
    let mut code_refs: Vec<String> = Vec::new();
    let mut in_backticks = false;
    let mut buf = String::new();
    // Backtick-delimited code refs.
    for ch in doc.chars() {
        if ch == '`' {
            if in_backticks && !buf.is_empty() {
                code_refs.push(buf.clone());
            }
            buf.clear();
            in_backticks = !in_backticks;
        } else if in_backticks {
            buf.push(ch);
        }
    }
    // Title-case or all-caps acronym words of length >= 3, outside backticks.
    let mut depth = 0;
    for word in doc.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.contains('`') {
            depth = if depth == 0 { 1 } else { 0 };
            continue;
        }
        if word.len() < 3 {
            continue;
        }
        // SAFETY: `word.len() >= 3` was just checked, so the word is non-empty
        // and `chars().next()` cannot be None.
        let Some(first) = word.chars().next() else {
            continue;
        };
        let all_upper = word
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let title =
            first.is_ascii_uppercase() && word.chars().skip(1).any(|c| c.is_ascii_lowercase());
        if all_upper || title {
            keywords.push(word.to_string());
        }
    }
    keywords.sort();
    keywords.dedup();
    code_refs.sort();
    code_refs.dedup();
    (keywords, code_refs)
}

/// Apply Rust / Scala / PHP method-name qualification to `name`, returning the
/// possibly-qualified form. Falls back to the input when no enclosing container
/// is found.
fn qualify_method_name(
    lang: &str,
    chunk_type: &ChunkType,
    node: Node<'_>,
    src: &[u8],
    name: String,
) -> String {
    if *chunk_type != ChunkType::Method || name.is_empty() {
        return name;
    }
    match lang {
        "rust" => {
            if let Some(ty) = rust_impl_type_name(node, src) {
                return format!("{ty}::{name}");
            }
        }
        "scala" => {
            if let Some(ty) = scala_enclosing_class_name(node, src) {
                if !ty.is_empty() {
                    return format!("{ty}::{name}");
                }
            }
        }
        "php" => {
            if let Some(ty) = php_enclosing_class_name(node, src) {
                if !ty.is_empty() {
                    return format!("{ty}::{name}");
                }
            }
        }
        _ => {}
    }
    name
}

/// True for AST node kinds that represent a function/method body — we never
/// descend through these because nested functions are emitted as their own
/// top-level chunks.
fn is_function_body_node(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_declaration"
            | "function_definition"
            | "method_declaration"
            | "method_definition"
            | "constructor_declaration"
            | "secondary_constructor"
            | "init_declaration"
            | "protocol_function_declaration"
    )
}

/// Ceiling on raw tree-sitter AST depth (distinct from `chunk_depth`, which
/// only increments for *classified* container nodes — see below). Past this,
/// a subtree stops being descended into and the file is logged as truncated
/// rather than continuing to pay the cost of walking arbitrarily deep,
/// pathological structure.
///
/// Why this exists in addition to the iterative rewrite: converting the walk
/// to iteration removes the stack-overflow crash (issue #3537) regardless of
/// depth, but `classify_node` calls `Node::parent()` on every visited node,
/// and tree-sitter's `parent()` is not O(1) — it re-derives ancestry from the
/// tree, so it costs roughly O(depth). Visiting every node of an
/// N-deep pathological chain then costs O(N) calls at up to O(N) each, i.e.
/// O(N^2) total — confirmed experimentally (a synthetic 5,000-deep nested
/// expression took 16s+ of wall time despite tree-sitter *parsing* the same
/// input in ~20ms). That's a real world hang, not just a theoretical one, for
/// the "deeply templated headers" / deeply nested generated-code shape this
/// issue describes. The depth cap bounds that cost to a constant regardless
/// of how deep the input actually nests.
///
/// 1000 is comfortably past any legitimate nesting seen in real code (deeply
/// nested closures/match arms/blocks rarely exceed a few dozen levels) while
/// keeping worst-case per-file walk cost small and constant.
const MAX_WALK_DEPTH: usize = 1000;

/// Tree walk that emits one `RawChunk` per classified node.
///
/// Iterative (explicit heap-allocated work stack) rather than recursive: the
/// walk depth tracks the tree-sitter parse-tree depth, which is attacker
/// influenced (it comes directly from file content being indexed). A native
/// recursive descent here could blow the process stack on pathological input
/// — e.g. deeply nested template arguments or a deeply parenthesized
/// global-scope expression in a generated/templated C++ header — and crash
/// the whole daemon, not just the one file being indexed (issue #3537).
/// Mirrors the explicit-stack pattern `collect_calls` already uses in this
/// file, and removes the failure mode entirely rather than merely raising
/// the stack-size ceiling. Combined with `MAX_WALK_DEPTH` above so a
/// pathological file degrades to "partially chunked, logged" rather than
/// either crashing or hanging the indexing worker.
pub(super) fn walk_for_chunks(
    root: Node<'_>,
    src: &[u8],
    file: &str,
    lang: &str,
    line_offsets: &[usize],
    depth: usize,
    out: &mut Vec<RawChunk>,
) {
    // (node, chunk_depth-for-this-node, raw AST depth from `root`).
    // `chunk_depth` only increments when descending into a *classified*
    // container (see push sites below) — it does not bound raw AST depth for
    // long chains of unclassified nodes (e.g. a deeply parenthesized
    // expression), which is exactly the shape that originally crashed the
    // daemon. `raw_depth` tracks true tree depth unconditionally and is what
    // `MAX_WALK_DEPTH` is checked against.
    //
    // Children are pushed in reverse order so the leftmost child is popped
    // (and therefore fully processed, including its own descendants) first —
    // this reproduces the same pre-order, left-to-right emission sequence
    // the original recursive walk produced.
    let mut stack: Vec<(Node<'_>, usize, usize)> = vec![(root, depth, 0)];
    let mut depth_capped = false;

    while let Some((node, depth, raw_depth)) = stack.pop() {
        if raw_depth > MAX_WALK_DEPTH {
            if !depth_capped {
                depth_capped = true;
                tracing::warn!(
                    file,
                    max_depth = MAX_WALK_DEPTH,
                    "chunker: AST nesting exceeds max walk depth; chunking was \
                     truncated past this depth for this file (issue #3537 guard)"
                );
            }
            continue;
        }

        // Try to classify this node.
        if let Some(chunk_type) = classify_node(lang, node) {
            let start_byte = node.start_byte();
            let end_byte = node.end_byte();
            let start_line = line_for_byte(line_offsets, start_byte);
            let end_line = line_for_byte(line_offsets, end_byte.saturating_sub(1));
            let content = std::str::from_utf8(&src[start_byte..end_byte])
                .unwrap_or("")
                .to_string();
            let name = qualify_method_name(lang, &chunk_type, node, src, name_of(node, src));

            let calls = collect_calls(node, src, lang);
            let inherits_from = collect_inherits(node, src, lang);
            let doc = preceding_doc_comments(node, src);
            let (nlp_keywords, nlp_code_refs) = nlp_from_doc(&doc);

            let id = make_chunk_id(file, &chunk_type, &name, start_line, end_line);
            out.push(RawChunk {
                id,
                file: file.to_string(),
                start_line,
                end_line,
                content,
                function_name: if name.is_empty() { None } else { Some(name) },
                language: Some(lang.to_string()),
                chunk_type,
                calls,
                inherits_from,
                chunk_depth: depth,
                parent_chunk_id: None,
                child_chunk_ids: Vec::new(),
                nlp_keywords,
                nlp_code_refs,
                virtual_terms: Vec::new(),
            });

            // Descend into impl/class/module to capture their methods/inner
            // items, but don't recurse into function/method bodies (no
            // inner-fn chunks).
            if !is_function_body_node(node.kind()) {
                let mut cursor = node.walk();
                let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
                for child in children.into_iter().rev() {
                    stack.push((child, depth + 1, raw_depth + 1));
                }
            }
            continue;
        }

        // Not a chunk-producing node: continue walking at the same chunk_depth.
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, depth, raw_depth + 1));
        }
    }
}

/// If a chunk exceeds `MAX_CHUNK_LINES`, replace it with sliding sub-chunks
/// that keep `parent_chunk_id` pointing back at the AST chunk.
pub(super) fn split_oversized(chunks: Vec<RawChunk>) -> Vec<RawChunk> {
    let mut out: Vec<RawChunk> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let line_count = chunk.end_line.saturating_sub(chunk.start_line) + 1;
        if line_count <= MAX_CHUNK_LINES {
            out.push(chunk);
            continue;
        }

        let parent_id = chunk.id.clone();
        let mut child_ids: Vec<String> = Vec::new();

        let lines: Vec<&str> = chunk.content.lines().collect();
        let mut start = 0usize;
        let mut sub_idx = 0usize;
        while start < lines.len() {
            let end = (start + SUB_CHUNK_WINDOW).min(lines.len());
            let text = lines[start..end].join("\n");
            let sub_id = format!("{parent_id}::sub::{sub_idx}");
            child_ids.push(sub_id.clone());
            out.push(RawChunk {
                id: sub_id,
                file: chunk.file.clone(),
                start_line: chunk.start_line + start,
                end_line: chunk.start_line + end - 1,
                content: text,
                function_name: chunk.function_name.clone(),
                language: chunk.language.clone(),
                chunk_type: chunk.chunk_type.clone(),
                calls: Vec::new(),
                inherits_from: Vec::new(),
                chunk_depth: chunk.chunk_depth,
                parent_chunk_id: Some(parent_id.clone()),
                child_chunk_ids: Vec::new(),
                nlp_keywords: Vec::new(),
                nlp_code_refs: Vec::new(),
                virtual_terms: Vec::new(),
            });
            if end == lines.len() {
                break;
            }
            start += SUB_CHUNK_STRIDE;
            sub_idx += 1;
        }

        // Keep the umbrella parent chunk too, with its child IDs filled in.
        let mut parent = chunk;
        parent.child_chunk_ids = child_ids;
        out.push(parent);
    }
    out
}

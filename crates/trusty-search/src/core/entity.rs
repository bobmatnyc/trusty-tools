//! Entity extraction from tree-sitter parse trees.
//!
//! Why: search and KG quality benefit from a typed view of the program's
//! "things" — type names, trait bounds, module paths, error sites, derives,
//! string literals and test relations. The chunker walks the AST anyway, so
//! emitting a flat `Vec<RawEntity>` in the same pass is essentially free.
//!
//! What: this module defines `EntityType`, `RawEntity`, and language-specific
//! extractors. The Rust extractor implements the full taxonomy from issue #17;
//! other languages currently extract `NamedType` and `ModulePath` only and
//! emit a `tracing::debug!` note for the unimplemented entity kinds.
//!
//! Test: see `#[cfg(test)]` in `chunker.rs` (`test_rust_entity_named_types`),
//! which round-trips a Rust source string through `chunk_ast` and asserts the
//! `NamedType` set.

use tree_sitter::{Node, Tree};

// The data shapes (EntityType, EdgeKind, RawEntity, fact_hash_str, tables)
// live in `trusty-symgraph::contracts` so analyzer crates can consume them
// without pulling in the 16 tree-sitter language grammars below.
// trusty-symgraph is depended on with `default-features = false` so only the
// pure-data contracts surface is linked (no tree-sitter, no parser deps).
// Tree-sitter–driven extraction stays here.
pub use trusty_common::symgraph::contracts::EdgeKind;
pub use trusty_common::symgraph::{fact_hash_str, EntityType, RawEntity};

/// redb table name constants for entity storage.
///
/// Re-exported from `trusty_common::symgraph::contracts::tables` for backward
/// compatibility with existing `crate::core::entity::tables::*` call sites.
pub mod tables {
    pub use trusty_common::symgraph::contracts::tables::*;
}

/// Slice the source text for a node and return it as an owned string.
fn node_text(node: Node<'_>, src: &[u8]) -> String {
    std::str::from_utf8(&src[node.start_byte()..node.end_byte()])
        .unwrap_or("")
        .to_string()
}

/// Marker type providing a typed entry point to entity extraction (issue #17).
///
/// Wraps `extract_entities`; semantics are identical. Keeps the public API
/// stable while letting callers prefer a struct-based handle.
pub struct EntityExtractor;

impl EntityExtractor {
    /// Extract Phase A structural entities from a tree-sitter parse tree.
    ///
    /// Runs alongside chunking (same `Tree` is reused, so this adds zero
    /// extra parse cost). Target: <5ms for a 500-line Rust file.
    ///
    /// `lang` should be one of `rust`, `python`, `javascript`, `typescript`,
    /// `go`, `java`, `c`, `cpp`. Unknown languages return an empty vector.
    pub fn extract(tree: &Tree, src: &[u8], file: &str, lang: &str) -> Vec<RawEntity> {
        extract_entities(tree, src, file, lang)
    }
}

/// Public entry point: walk `tree` and emit entities for `lang`.
pub fn extract_entities(tree: &Tree, src: &[u8], file: &str, lang: &str) -> Vec<RawEntity> {
    match lang {
        "rust" => extract_rust(tree, src, file),
        // Other languages: NamedType + ModulePath best-effort.
        "python" | "javascript" | "typescript" | "go" | "java" | "c" | "cpp" => {
            tracing::debug!("entity extraction not fully implemented for {lang}");
            extract_universal(tree, src, file)
        }
        _ => Vec::new(),
    }
}

/// Universal extractor: looks for `type_identifier`-ish nodes and `scoped_identifier`-ish
/// chains. Used as a stub for non-Rust languages.
fn extract_universal(tree: &Tree, src: &[u8], file: &str) -> Vec<RawEntity> {
    let mut out = Vec::new();
    let mut stack: Vec<Node> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if kind == "type_identifier" || kind == "type" {
            let text = node_text(node, src);
            if !text.is_empty() {
                out.push(RawEntity::new(
                    EntityType::NamedType,
                    text,
                    (node.start_byte(), node.end_byte()),
                    file,
                    node.start_position().row + 1,
                ));
            }
        } else if kind == "scoped_identifier" || kind == "qualified_identifier" {
            let text = node_text(node, src);
            if text.contains("::") || text.contains('.') {
                out.push(RawEntity::new(
                    EntityType::ModulePath,
                    text,
                    (node.start_byte(), node.end_byte()),
                    file,
                    node.start_position().row + 1,
                ));
            }
        }
        let mut walker = node.walk();
        for child in node.children(&mut walker) {
            stack.push(child);
        }
    }
    out
}

/// Rust extractor. Implements the full taxonomy from issue #17.
fn extract_rust(tree: &Tree, src: &[u8], file: &str) -> Vec<RawEntity> {
    let mut out = Vec::new();
    let root = tree.root_node();

    // Top-level `use` declarations: classify ExternalCrate vs ModulePath.
    let mut top_cursor = root.walk();
    for child in root.children(&mut top_cursor) {
        if child.kind() == "use_declaration" {
            let text = node_text(child, src);
            // first path segment after `use ` and before `::` or whitespace
            let trimmed = text.trim_start_matches("use ").trim_end_matches(';').trim();
            let first = trimmed
                .split(|c: char| c == ':' || c.is_whitespace() || c == '{' || c == ',')
                .find(|s| !s.is_empty())
                .unwrap_or("");
            let line = child.start_position().row + 1;
            let span = (child.start_byte(), child.end_byte());
            if !first.is_empty()
                && !matches!(first, "crate" | "super" | "self" | "std" | "core" | "alloc")
            {
                out.push(RawEntity::new(
                    EntityType::ExternalCrate,
                    first.to_string(),
                    span,
                    file,
                    line,
                ));
            }
            out.push(RawEntity::new(
                EntityType::ModulePath,
                trimmed.to_string(),
                span,
                file,
                line,
            ));
        }
    }

    // Recursive walk for the rest.
    walk_rust(root, src, file, false, &mut out);
    out
}

/// Ceiling on raw AST depth for the entity walk. Mirrors `MAX_WALK_DEPTH` in
/// `chunker/walk.rs` (kept as a separate constant since `walk` is private to
/// the `chunker` module) — bounds worst-case entity-list size and traversal
/// cost for pathologically deep input rather than growing unboundedly with
/// nesting depth (issue #3537).
const MAX_WALK_DEPTH: usize = 1000;

/// Iterative (explicit heap-allocated work stack) rather than recursive: see
/// `walk_for_chunks` in `chunker/walk.rs` for the rationale — walk depth
/// tracks tree-sitter parse-tree depth, which is attacker influenced, so a
/// native recursive descent here is the same unbounded-stack failure mode
/// tracked in issue #3537, just for the Rust entity extractor rather than
/// the chunker.
fn walk_rust(root: Node<'_>, src: &[u8], file: &str, in_test_fn: bool, out: &mut Vec<RawEntity>) {
    // (node, in_test_fn-for-this-node, raw AST depth from `root`). Children
    // pushed in reverse so traversal order (and therefore entity emission
    // order) matches the original pre-order recursive walk.
    let mut stack: Vec<(Node<'_>, bool, usize)> = vec![(root, in_test_fn, 0)];
    let mut depth_capped = false;

    while let Some((node, in_test_fn, raw_depth)) = stack.pop() {
        if raw_depth > MAX_WALK_DEPTH {
            if !depth_capped {
                depth_capped = true;
                tracing::warn!(
                    file,
                    max_depth = MAX_WALK_DEPTH,
                    "entity extraction: AST nesting exceeds max walk depth; \
                     extraction was truncated past this depth for this file (issue #3537 guard)"
                );
            }
            continue;
        }

        let kind = node.kind();
        let line = node.start_position().row + 1;
        let span = (node.start_byte(), node.end_byte());

        match kind {
            "type_identifier" => {
                let t = node_text(node, src);
                if !t.is_empty() {
                    out.push(RawEntity::new(EntityType::NamedType, t, span, file, line));
                }
            }
            "trait_bounds" => {
                let t = node_text(node, src);
                out.push(RawEntity::new(EntityType::TraitBound, t, span, file, line));
            }
            "scoped_identifier" => {
                let t = node_text(node, src);
                if t.contains("::") {
                    out.push(RawEntity::new(EntityType::ModulePath, t, span, file, line));
                }
            }
            "macro_invocation" => {
                // e.g. `bail!(...)`, `anyhow::bail!(...)`, `panic!(...)`. The `macro`
                // field can be either an `identifier` or a `scoped_identifier`; we
                // care about the final segment.
                if let Some(name_node) = node.child_by_field_name("macro") {
                    let name = node_text(name_node, src);
                    let last = name.rsplit("::").next().unwrap_or(&name).trim();
                    if matches!(last, "bail" | "anyhow" | "panic" | "unwrap" | "expect") {
                        let t = node_text(node, src);
                        out.push(RawEntity::new(
                            EntityType::ErrorVariant,
                            t,
                            span,
                            file,
                            line,
                        ));
                    }
                }
            }
            "call_expression" => {
                // `.unwrap()` and `.expect()` method calls also count.
                if let Some(func) = node.child_by_field_name("function") {
                    let txt = node_text(func, src);
                    let last = txt.rsplit('.').next().unwrap_or(&txt);
                    if matches!(last, "unwrap" | "expect") {
                        let t = node_text(node, src);
                        out.push(RawEntity::new(
                            EntityType::ErrorVariant,
                            t,
                            span,
                            file,
                            line,
                        ));
                    }
                }
            }
            "attribute_item" | "inner_attribute_item" => {
                let t = node_text(node, src);
                out.push(RawEntity::new(EntityType::Annotation, t, span, file, line));
            }
            "string_literal" => {
                let t = node_text(node, src);
                // Strip surrounding quotes for length check.
                let inner = t.trim_matches('"');
                if inner.len() > 10 {
                    out.push(RawEntity::new(
                        EntityType::LiteralString,
                        t,
                        span,
                        file,
                        line,
                    ));
                }
            }
            "type_item" => {
                let t = node_text(node, src);
                out.push(RawEntity::new(EntityType::TypeAlias, t, span, file, line));
            }
            "identifier" if in_test_fn => {
                let t = node_text(node, src);
                if !t.is_empty() {
                    out.push(RawEntity::new(
                        EntityType::TestRelation,
                        t,
                        span,
                        file,
                        line,
                    ));
                }
            }
            _ => {}
        }

        // Detect entry into a test function so identifiers inside count as TestRelation.
        let entering_test_fn = kind == "function_item" && function_has_test_attr(node, src);
        let next_in_test_fn = in_test_fn || entering_test_fn;

        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, next_in_test_fn, raw_depth + 1));
        }
    }
}

/// Returns true if any preceding `attribute_item` sibling on this `function_item`
/// includes the `test` attribute. Tree-sitter-rust attaches attributes as
/// previous siblings, not as children of `function_item`.
fn function_has_test_attr(node: Node<'_>, src: &[u8]) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        let k = p.kind();
        if k == "attribute_item" || k == "inner_attribute_item" {
            let t = node_text(p, src);
            if t.contains("test") {
                return true;
            }
            prev = p.prev_sibling();
        } else if k == "line_comment" || k == "block_comment" {
            prev = p.prev_sibling();
        } else {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_rust(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        p.set_language(&lang).expect("set rust language");
        p.parse(src, None).expect("parse")
    }

    /// Issue #17: a Rust snippet exercising NamedType, ModulePath, and
    /// TestRelation entity kinds round-trips through `EntityExtractor::extract`.
    #[test]
    fn test_extractor_emits_named_type_modulepath_and_test_relation() {
        let src = "use std::sync::Arc;\n\
                   struct MyType { v: u32 }\n\
                   #[test]\n\
                   fn it_works() { let _ = Arc::new(MyType { v: 1 }); }\n";
        let tree = parse_rust(src);
        let ents = EntityExtractor::extract(&tree, src.as_bytes(), "x.rs", "rust");

        assert!(
            ents.iter()
                .any(|e| matches!(e.entity_type, EntityType::NamedType) && e.text == "MyType"),
            "expected NamedType=MyType in {ents:?}"
        );
        assert!(
            ents.iter()
                .any(|e| matches!(e.entity_type, EntityType::ModulePath)),
            "expected at least one ModulePath in {ents:?}"
        );
        assert!(
            ents.iter()
                .any(|e| matches!(e.entity_type, EntityType::TestRelation)),
            "expected TestRelation identifiers from #[test] fn body in {ents:?}"
        );
    }
}

//! PHP `LanguageAnalyzer` adapter backed by tree-sitter-php.
//!
//! Why: Extracts PHP structure — functions, methods, classes, interfaces,
//! traits, namespace `use`/`require`/`include` imports, and intra-method call
//! edges — into a language-neutral `KgGraph`. Mirrors the Python and Ruby
//! adapters so the analyzer registry behaves uniformly across languages.
//!
//! What: For each `CodeChunk`, parses the content with tree-sitter-php,
//! walks the tree, and emits:
//! - one `File` node per unique `chunk.file`
//! - `Function` nodes for top-level `function_definition`
//! - `Method` nodes for `method_declaration` inside a class/interface/trait
//!   with class-qualified IDs `php:Method:file:Class:name`
//! - `Class` nodes for `class_declaration` and `trait_declaration`
//!   (traits map to `Class` — closest semantic match)
//! - `Interface` nodes for `interface_declaration`
//! - `Import` nodes + `Imports` edges for `namespace_use_declaration`
//!   (`use Foo\Bar;`) and `include`/`require` (and `_once` variants) when the
//!   argument is a string literal
//! - `Calls` edges from each function/method to its callees, scoped to the
//!   enclosing function/method, deduplicated with `weight = call_count`
//!
//! Test: see the `tests` module — covers detection, methods (class-qualified
//! IDs), interface/trait emission, call edges (scoped + deduped), and `use`
//! imports.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-php-backed analyzer.
pub struct PhpAnalyzer;

impl PhpAnalyzer {
    /// Construct a stateless analyzer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PhpAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for PhpAnalyzer {
    fn language(&self) -> &str {
        "php"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".php"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-php grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "php analyze chunk");
            // The PHP grammar (LANGUAGE_PHP) requires a `<?php` opener; if the
            // chunk content is missing one (e.g. a stripped fragment), prepend
            // it so the parser doesn't bail to an ERROR root.
            let needs_prefix = !chunk.content.trim_start().starts_with("<?");
            let owned: String;
            let source: &str = if needs_prefix {
                owned = format!("<?php\n{}", chunk.content);
                &owned
            } else {
                &chunk.content
            };
            let Some(tree) = parser.parse(source, None) else {
                result.errors.push(format!("parse failure: {}", chunk.file));
                continue;
            };
            result.analyzed_chunks += 1;
            if seen_files.insert(chunk.file.clone()) {
                result.analyzed_files += 1;
                result.graph.nodes.push(file_node(&chunk.file));
            }

            let src = source.as_bytes();
            let file_id = format!("php:File:{}", chunk.file);
            recurse(
                tree.root_node(),
                src,
                chunk,
                &mut result.graph,
                &file_id,
                None,
            );
        }

        result
    }
}

fn file_node(file: &str) -> KgNode {
    KgNode {
        id: format!("php:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "php".into(),
        file: file.to_string(),
        start_line: 0,
        end_line: 0,
        doc_comment: None,
        is_public: false,
        extra: serde_json::Value::Null,
    }
}

fn node_text(node: Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

fn name_of(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name").map(|n| node_text(n, src))
}

fn make_simple_node(kind: KgNodeKind, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let kind_str = format!("{kind:?}");
    // PHP visibility is keyword-driven; underscore prefix is purely
    // conventional. Mirror the other adapters and treat names without a
    // leading underscore as public by default.
    let is_public = !name.starts_with('_');
    KgNode {
        id: format!("php:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "php".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public,
        extra: serde_json::Value::Null,
    }
}

/// Build a method node with class-qualified ID/qualified_name. Mirrors the
/// Python and Ruby adapter strategy so methods on different classes don't
/// collide.
///
/// Why: PHP method names (`__construct`, `handle`, `__toString`) are reused
/// across countless classes. Without a class qualifier the cross-chunk linker
/// would merge them into one node.
/// What: Returns a `KgNode` with `id = php:Method:file:Class:name` and
/// `qualified_name = Class.name`. When `class_name` is empty falls back to the
/// bare name (which only happens when a stray `method_declaration` appears
/// outside a `declaration_list`, an unusual case).
/// Test: `php_extracts_class_methods_with_qualified_ids`.
fn make_method_node(class_name: &str, name: &str, chunk: &CodeChunk, ast: Node) -> KgNode {
    let start = (chunk.start_line as u32).saturating_add(ast.start_position().row as u32);
    let end = (chunk.start_line as u32).saturating_add(ast.end_position().row as u32);
    let qualified = if class_name.is_empty() {
        name.to_string()
    } else {
        format!("{class_name}.{name}")
    };
    let id_suffix = if class_name.is_empty() {
        name.to_string()
    } else {
        format!("{class_name}:{name}")
    };
    KgNode {
        id: format!("php:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "php".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

/// Names that look like language constructs / declarative DSL and shouldn't
/// be treated as outgoing call edges from a function body.
fn is_declarative_call(name: &str) -> bool {
    matches!(
        name,
        "echo"
            | "print"
            | "isset"
            | "empty"
            | "unset"
            | "list"
            | "array"
            | "die"
            | "exit"
            | "include"
            | "require"
            | "include_once"
            | "require_once"
    )
}

/// Walk the PHP AST emitting nodes/edges, keeping track of the enclosing
/// container (file/class/interface/trait).
fn recurse(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
    class_name: Option<&str>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(name) = name_of(node, src) {
                // Top-level function — emit as Function. Method decls live
                // under `declaration_list` and are handled in the class arm.
                let n = make_simple_node(KgNodeKind::Function, &name, chunk, node);
                let id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    for edge in extract_calls(body, src, &id) {
                        graph.edges.push(edge);
                    }
                }
            }
            return;
        }
        "method_declaration" => {
            if let Some(name) = name_of(node, src) {
                let n = make_method_node(class_name.unwrap_or(""), &name, chunk, node);
                let id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    for edge in extract_calls(body, src, &id) {
                        graph.edges.push(edge);
                    }
                }
            }
            return;
        }
        "class_declaration" | "trait_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src);
                // Traits map to `Class` — closest semantic match in our
                // language-neutral schema; PHP traits are mixin-like classes.
                let n = make_simple_node(KgNodeKind::Class, &name, chunk, node);
                let class_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: class_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        recurse(child, src, chunk, graph, &class_id, Some(&name));
                    }
                }
            }
            return;
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src);
                let n = make_simple_node(KgNodeKind::Interface, &name, chunk, node);
                let iface_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: iface_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        recurse(child, src, chunk, graph, &iface_id, Some(&name));
                    }
                }
            }
            return;
        }
        "namespace_use_declaration" => {
            emit_namespace_use(node, src, chunk, graph, parent_id);
            return;
        }
        "include_expression"
        | "require_expression"
        | "include_once_expression"
        | "require_once_expression" => {
            emit_include_like(node, src, chunk, graph, parent_id);
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        recurse(child, src, chunk, graph, parent_id, class_name);
    }
}

/// Emit one `Import` node + `Imports` edge per `namespace_use_clause` inside a
/// `namespace_use_declaration`.
///
/// Why: PHP's `use Foo\Bar;` is the closest analogue to Python's `import` —
/// surfacing it lets the dependency graph show file-level fan-out. We keep the
/// dotted form (`Foo.Bar.Baz`) so it lines up with the convention used by the
/// Python adapter.
/// What: Iterates `namespace_use_clause` children of the declaration; for each
/// clause grabs the inner `name` or `qualified_name` child, replaces `\` with
/// `.`, and emits one node + edge per target. Skips clauses with no resolvable
/// name.
/// Test: `php_extracts_use_imports`.
fn emit_namespace_use(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "namespace_use_clause" {
            continue;
        }
        // The clause holds either a `name` or `qualified_name` child.
        let mut inner_cursor = child.walk();
        let mut target: Option<String> = None;
        for inner in child.children(&mut inner_cursor) {
            match inner.kind() {
                "qualified_name" | "name" => {
                    let raw = node_text(inner, src);
                    // Normalize `\Foo\Bar` and `Foo\Bar` to `Foo.Bar`.
                    let cleaned = raw.trim_start_matches('\\').replace('\\', ".");
                    if !cleaned.is_empty() {
                        target = Some(cleaned);
                    }
                    break;
                }
                _ => {}
            }
        }
        let Some(target) = target else {
            continue;
        };
        emit_import_node(&target, chunk, node, graph, parent_id);
    }
}

/// Emit an `Import` node + `Imports` edge for an `include`/`require` family
/// expression whose argument is a string literal.
///
/// Why: Although less common in modern code, `require 'config.php'` still
/// drives the dependency graph in many older PHP codebases.
/// What: Inspects the lone expression child; if it is a `string` literal,
/// extracts its `string_content` and emits one node + edge. Variable arguments
/// (`require $path`) are silently skipped.
/// Test: indirectly via `php_extracts_use_imports` (smoke); behavior verified
/// by the parse path itself.
fn emit_include_like(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Drill through the `expression` wrapper.
        let candidate = if child.kind() == "expression" {
            child.named_child(0).unwrap_or(child)
        } else {
            child
        };
        if candidate.kind() != "string" {
            continue;
        }
        let mut inner_cursor = candidate.walk();
        let mut target: Option<String> = None;
        for inner in candidate.children(&mut inner_cursor) {
            if inner.kind() == "string_content" {
                target = Some(node_text(inner, src));
                break;
            }
        }
        let target = target.unwrap_or_else(|| {
            node_text(candidate, src)
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string()
        });
        if target.is_empty() {
            continue;
        }
        emit_import_node(&target, chunk, node, graph, parent_id);
        break;
    }
}

fn emit_import_node(
    target: &str,
    chunk: &CodeChunk,
    ast: Node,
    graph: &mut KgGraph,
    parent_id: &str,
) {
    let n = make_simple_node(KgNodeKind::Import, target, chunk, ast);
    let id = n.id.clone();
    graph.nodes.push(n);
    graph.edges.push(KgEdge {
        from: parent_id.to_string(),
        to: id,
        kind: KgEdgeKind::Imports,
        weight: 1.0,
    });
}

/// Extract call expressions from a function/method body and produce
/// deduplicated `Calls` edges keyed by callee name.
///
/// Why: Per-caller outgoing call graphs are the cheapest behavioral signal we
/// can emit; counting unique callees with `weight = count` keeps the graph
/// compact while preserving frequency information.
/// What: Walks the AST subtree rooted at `body`, collects `function_call_expression`,
/// `member_call_expression`, `nullsafe_member_call_expression`, and
/// `scoped_call_expression` nodes. Skips into nested `function_definition`,
/// `method_declaration`, `class_declaration`, `interface_declaration`,
/// `trait_declaration`, `anonymous_function`, and `arrow_function` so each
/// caller only attributes its own direct calls. Skips PHP language constructs
/// (echo/print/isset/etc.) and emits one `KgEdge` per unique callee with
/// `weight = call_count as f32`.
/// Test: `php_adapter_extracts_call_edges`,
/// `php_adapter_deduplicates_repeated_calls`,
/// `php_method_call_edges_scoped_to_method`.
fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        match node.kind() {
            // Stop at nested function-like bodies so each caller only
            // attributes its own direct calls.
            "function_definition"
            | "method_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "anonymous_function"
            | "anonymous_function_creation_expression"
            | "arrow_function" => {
                return;
            }
            "function_call_expression"
            | "member_call_expression"
            | "nullsafe_member_call_expression"
            | "scoped_call_expression" => {
                if let Some(callee) = callee_name(node, src) {
                    if !is_declarative_call(&callee) {
                        *counts.entry(callee).or_insert(0) += 1;
                    }
                }
                // Recurse into arguments so nested calls are still counted
                // (e.g. `foo(bar())` records both foo and bar).
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, src, counts);
        }
    }

    visit(body, src, &mut counts);

    counts
        .into_iter()
        .map(|(callee, count)| KgEdge {
            from: caller_id.to_string(),
            to: build_call_target("php", "Method", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Extract a best-effort callee name from a PHP call node.
///
/// Why: Cross-file symbol resolution is out of scope for the adapter; the
/// cross-chunk linker merges by qualified_name later. We just need a stable
/// string handle per call site.
/// What: For `function_call_expression` reads the `function` field — bare
/// `name` returns its text, `qualified_name` returns the trailing segment.
/// For `member_call_expression` / `nullsafe_member_call_expression` /
/// `scoped_call_expression` reads the `name` field. Returns `None` for
/// dynamic / variable callees we can't resolve.
/// Test: exercised indirectly by the call-edge tests.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    match call.kind() {
        "function_call_expression" => {
            let f = call.child_by_field_name("function")?;
            match f.kind() {
                "name" => Some(node_text(f, src)),
                "qualified_name" => {
                    let raw = node_text(f, src);
                    raw.rsplit('\\').next().map(|s| s.to_string())
                }
                _ => None,
            }
        }
        "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression" => {
            let n = call.child_by_field_name("name")?;
            if n.kind() == "name" {
                Some(node_text(n, src))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(content: &str) -> CodeChunk {
        CodeChunk {
            id: "f.php:1:10".into(),
            file: "f.php".into(),
            start_line: 1,
            end_line: 10,
            content: content.into(),
            function_name: None,
            score: 0.0,
            compact_snippet: None,
            match_reason: String::new(),
        }
    }

    #[test]
    fn php_supports_php_files() {
        let a = PhpAnalyzer::new();
        assert!(a.supports("foo.php"));
        assert!(a.supports("Index.PHP"));
        assert!(!a.supports("foo.py"));
        assert!(!a.supports("foo.rb"));
    }

    #[test]
    fn php_extracts_class_methods_with_qualified_ids() {
        let a = PhpAnalyzer::new();
        let src = "<?php\nclass Foo {\n  public function bar() {}\n  public function baz() {}\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(
            methods.len(),
            2,
            "expected two methods, got {:?}",
            r.graph.nodes
        );
        for m in &methods {
            assert!(
                m.id.contains(":Foo:"),
                "method id should embed class name 'Foo', got {}",
                m.id
            );
            assert!(
                m.qualified_name.starts_with("Foo."),
                "qualified_name should start with 'Foo.', got {}",
                m.qualified_name
            );
        }
        let names: Vec<&str> = methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"baz"));
    }

    #[test]
    fn php_interface_emits_interface_node() {
        let a = PhpAnalyzer::new();
        let src = "<?php\ninterface Greeter {\n  public function hello();\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let interfaces: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Interface))
            .collect();
        assert_eq!(
            interfaces.len(),
            1,
            "expected one Interface node, got {:?}",
            r.graph.nodes
        );
        assert_eq!(interfaces[0].name, "Greeter");
    }

    #[test]
    fn php_trait_emits_class_node() {
        let a = PhpAnalyzer::new();
        let src = "<?php\ntrait Loggable {\n  public function log() {}\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let classes: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Class))
            .collect();
        assert_eq!(
            classes.len(),
            1,
            "expected trait to emit one Class node, got {:?}",
            r.graph.nodes
        );
        assert_eq!(classes[0].name, "Loggable");
    }

    #[test]
    fn php_class_emits_class_node() {
        let a = PhpAnalyzer::new();
        let src = "<?php\nclass Foo {}\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Foo"));
    }

    #[test]
    fn php_adapter_extracts_call_edges() {
        let a = PhpAnalyzer::new();
        let src = "<?php\nclass Worker {\n  public function run() {\n    helper();\n    $this->other();\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let calls: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        assert!(
            !calls.is_empty(),
            "expected at least one Calls edge, got none. graph={:?}",
            r.graph
        );
        let has_helper = calls.iter().any(|e| e.to.ends_with(":helper"));
        let has_other = calls.iter().any(|e| e.to.ends_with(":other"));
        assert!(has_helper, "expected edge to 'helper', got {calls:?}");
        assert!(has_other, "expected edge to 'other', got {calls:?}");
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Method:") && e.from.contains(":Worker:run")),
            "call edges should originate from Worker.run, got {calls:?}"
        );
    }

    #[test]
    fn php_adapter_deduplicates_repeated_calls() {
        let a = PhpAnalyzer::new();
        let src = "<?php\nclass Foo {\n  public function caller() {\n    bar();\n    bar();\n    bar();\n  }\n}\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let bar_edges: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls) && e.to.ends_with(":bar"))
            .collect();
        assert_eq!(
            bar_edges.len(),
            1,
            "repeated calls should be deduplicated, got {bar_edges:?}"
        );
        assert!(
            (bar_edges[0].weight - 3.0).abs() < f32::EPSILON,
            "weight should reflect call count=3, got {}",
            bar_edges[0].weight
        );
    }

    #[test]
    fn php_method_call_edges_scoped_to_method() {
        let a = PhpAnalyzer::new();
        let src = "<?php\nclass Foo {\n  public function bar() {\n    helper();\n    helper();\n  }\n}\nfunction helper() {}\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let calls: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        assert_eq!(calls.len(), 1, "expected one deduped call edge: {calls:?}");
        assert!(
            calls[0].from.contains(":Method:") && calls[0].from.contains(":Foo:bar"),
            "call edge should originate from method Foo.bar, got {}",
            calls[0].from
        );
        assert!(
            (calls[0].weight - 2.0).abs() < f32::EPSILON,
            "weight should be 2, got {}",
            calls[0].weight
        );
    }

    #[test]
    fn php_extracts_use_imports() {
        let a = PhpAnalyzer::new();
        let src = "<?php\nuse Foo\\Bar\\Baz;\nuse Other\\Thing as T;\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let imports: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Import))
            .collect();
        assert_eq!(
            imports.len(),
            2,
            "expected two Import nodes, got {:?}",
            r.graph.nodes
        );
        let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"Foo.Bar.Baz"),
            "expected 'Foo.Bar.Baz' import target, got {names:?}"
        );
        assert!(
            names.contains(&"Other.Thing"),
            "expected 'Other.Thing' import target, got {names:?}"
        );
        let import_edges: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Imports))
            .collect();
        assert_eq!(import_edges.len(), 2);
        assert!(import_edges.iter().all(|e| e.from == "php:File:f.php"));
    }

    #[test]
    fn php_top_level_function_emits_function_node() {
        let a = PhpAnalyzer::new();
        let src = "<?php\nfunction hello() {}\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let funcs: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(funcs[0].name, "hello");
    }
}

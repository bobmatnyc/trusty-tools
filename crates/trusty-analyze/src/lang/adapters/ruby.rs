//! Ruby `LanguageAnalyzer` adapter backed by tree-sitter-ruby.
//!
//! Why: Extracts Ruby structure — methods, singleton methods, classes,
//! modules, requires, and intra-method call edges — into a language-neutral
//! `KgGraph`. Mirrors the Python and TypeScript adapters so the analyzer
//! registry behaves uniformly across languages.
//!
//! What: For each `CodeChunk`, parses the content with tree-sitter-ruby,
//! walks the tree, and emits:
//! - one `File` node per unique `chunk.file`
//! - `Method` nodes for `method` (instance) nested in a class/module, with
//!   class-qualified IDs `ruby:Method:file:Class:name`
//! - `Method` nodes for `singleton_method` (`def self.foo`) with
//!   `qualified_name = Class.name`
//! - top-level `method` becomes a `Function`-equivalent `Method` node with
//!   bare name (no class prefix)
//! - `Class` nodes for `class`
//! - `Interface` nodes for `module` (closest semantic match in our schema)
//! - `Import` nodes + `Imports` edges for `require` / `require_relative`
//! - `Calls` edges from each method to its callees, scoped to the enclosing
//!   method, deduplicated with `weight = call_count`
//!
//! Test: see the `tests` module — covers detection, methods, singleton
//! methods, modules, call extraction, deduplication, and require imports.

use crate::lang::call_target::build_call_target;
use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
use tree_sitter::{Node, Parser};

/// tree-sitter-ruby-backed analyzer.
pub struct RubyAnalyzer;

impl RubyAnalyzer {
    /// Construct a stateless analyzer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RubyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for RubyAnalyzer {
    fn language(&self) -> &str {
        "ruby"
    }

    fn supported_extensions(&self) -> &[&str] {
        &[".rb"]
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> StaticAnalysisResult {
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .is_err()
        {
            return StaticAnalysisResult {
                errors: vec!["failed to load tree-sitter-ruby grammar".into()],
                ..Default::default()
            };
        }

        let mut result = StaticAnalysisResult::default();
        let mut seen_files = std::collections::HashSet::new();

        for chunk in chunks {
            tracing::debug!(file = %chunk.file, "ruby analyze chunk");
            let Some(tree) = parser.parse(&chunk.content, None) else {
                result.errors.push(format!("parse failure: {}", chunk.file));
                continue;
            };
            result.analyzed_chunks += 1;
            if seen_files.insert(chunk.file.clone()) {
                result.analyzed_files += 1;
                result.graph.nodes.push(file_node(&chunk.file));
            }

            let src = chunk.content.as_bytes();
            let file_id = format!("ruby:File:{}", chunk.file);
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
        id: format!("ruby:File:{file}"),
        kind: KgNodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        language: "ruby".into(),
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
    // Ruby visibility is dynamic; treat anything not starting with `_` as public.
    let is_public = !name.starts_with('_');
    KgNode {
        id: format!("ruby:{kind_str}:{}:{name}", chunk.file),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "ruby".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        is_public,
        extra: serde_json::Value::Null,
    }
}

/// Build a method node with class-qualified ID/qualified_name. Mirrors the
/// Python adapter's strategy so methods on different classes don't collide.
///
/// Why: Ruby method names are short and frequently shared across classes
/// (e.g. `initialize`, `to_s`, `call`); without a class qualifier the linker
/// would merge them.
/// What: Returns a `KgNode` with `id = ruby:Method:file:Class:name` and
/// `qualified_name = Class.name`. When `class_name` is empty, falls back to
/// the bare name (top-level `def`).
/// Test: `ruby_extracts_class_methods_with_qualified_ids`,
/// `ruby_singleton_method_uses_class_qualified_name`.
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
        id: format!("ruby:Method:{}:{id_suffix}", chunk.file),
        kind: KgNodeKind::Method,
        name: name.to_string(),
        qualified_name: qualified,
        language: "ruby".into(),
        file: chunk.file.clone(),
        start_line: start,
        end_line: end,
        doc_comment: None,
        // Ruby has no `_` privacy convention by default, but we mirror the
        // other adapters so internal helpers like `_internal` are flagged.
        is_public: !name.starts_with('_'),
        extra: serde_json::Value::Null,
    }
}

/// True if this `call` node is a `require` / `require_relative` invocation.
/// We treat these as imports rather than as call edges.
fn is_require_call(call: Node, src: &[u8]) -> bool {
    let Some(method) = call.child_by_field_name("method") else {
        return false;
    };
    if method.kind() != "identifier" {
        return false;
    }
    let name = node_text(method, src);
    name == "require" || name == "require_relative"
}

/// Names that look like declarative DSL (`attr_reader`, etc.) and shouldn't
/// be treated as outgoing call edges.
fn is_declarative_call(name: &str) -> bool {
    matches!(
        name,
        "require"
            | "require_relative"
            | "attr_reader"
            | "attr_writer"
            | "attr_accessor"
            | "private"
            | "public"
            | "protected"
            | "include"
            | "extend"
            | "prepend"
    )
}

/// Walk the Ruby AST emitting nodes/edges, keeping track of the enclosing
/// container (file/class/module).
fn recurse(
    node: Node,
    src: &[u8],
    chunk: &CodeChunk,
    graph: &mut KgGraph,
    parent_id: &str,
    class_name: Option<&str>,
) {
    match node.kind() {
        "method" => {
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
        "singleton_method" => {
            if let Some(name) = name_of(node, src) {
                // Resolve receiver text. `def self.foo` → object text "self";
                // `def Klass.foo` → object text "Klass". Use the enclosing
                // class name when receiver is `self`, else the receiver text.
                let receiver = node
                    .child_by_field_name("object")
                    .map(|n| node_text(n, src));
                let qualifier: String = match receiver.as_deref() {
                    Some("self") | None => class_name.unwrap_or("").to_string(),
                    Some(other) => other.to_string(),
                };
                let n = make_method_node(&qualifier, &name, chunk, node);
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
        "class" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src);
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
        "module" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src);
                // Modules map to Interface — closest semantic in KgNodeKind.
                let n = make_simple_node(KgNodeKind::Interface, &name, chunk, node);
                let module_id = n.id.clone();
                graph.nodes.push(n);
                graph.edges.push(KgEdge {
                    from: parent_id.to_string(),
                    to: module_id.clone(),
                    kind: KgEdgeKind::Contains,
                    weight: 1.0,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        recurse(child, src, chunk, graph, &module_id, Some(&name));
                    }
                }
            }
            return;
        }
        "call" if is_require_call(node, src) => {
            emit_require(node, src, chunk, graph, parent_id);
            return;
        }
        "call" => {
            // Fall through; only require-style top-level calls become edges
            // outside method bodies. Ordinary calls outside methods are not
            // attributed (no caller).
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        recurse(child, src, chunk, graph, parent_id, class_name);
    }
}

/// Emit one `Import` node + `Imports` edge for a `require` / `require_relative`
/// call.
///
/// Why: Ruby doesn't have dedicated import syntax; requires are parsed as
/// regular method calls. We pull them out so the dependency graph reflects
/// file-level loading just like Python's `import`.
/// What: Looks at the first string argument and emits one node per require.
/// Falls back silently if the argument shape is unexpected (e.g. dynamic
/// `require some_var`).
/// Test: `ruby_extracts_require_imports`.
fn emit_require(node: Node, src: &[u8], chunk: &CodeChunk, graph: &mut KgGraph, parent_id: &str) {
    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.kind() != "string" {
            continue;
        }
        // Find the inner string_content if present, otherwise strip quotes.
        let mut content_cursor = child.walk();
        let mut target: Option<String> = None;
        for inner in child.children(&mut content_cursor) {
            if inner.kind() == "string_content" {
                target = Some(node_text(inner, src));
                break;
            }
        }
        let target = target.unwrap_or_else(|| {
            let raw = node_text(child, src);
            raw.trim_matches(|c| c == '"' || c == '\'').to_string()
        });
        if target.is_empty() {
            continue;
        }
        let n = make_simple_node(KgNodeKind::Import, &target, chunk, node);
        let id = n.id.clone();
        graph.nodes.push(n);
        graph.edges.push(KgEdge {
            from: parent_id.to_string(),
            to: id,
            kind: KgEdgeKind::Imports,
            weight: 1.0,
        });
        // Only emit one import per require call (require takes one arg).
        break;
    }
}

/// Extract `call` expression nodes from a method body and produce
/// deduplicated `Calls` edges keyed by callee name.
///
/// Why: Per-method outgoing call graphs are the cheapest behavioral signal we
/// can emit; unique-per-callee deduplication with `weight = count` keeps the
/// graph compact while preserving frequency information.
/// What: Walks the AST subtree rooted at `body`, collects every direct `call`
/// (skipping nested `method` / `singleton_method` / `class` / `module` /
/// `do_block` / `block` bodies so each method only attributes its own direct
/// calls), resolves the callee name, skips declarative DSL like `attr_reader`,
/// counts repeats, and returns one `KgEdge` per unique callee with
/// `weight = call_count as f32`.
/// Test: `ruby_adapter_extracts_call_edges`,
/// `ruby_adapter_deduplicates_repeated_calls`.
fn extract_calls(body: Node, src: &[u8], caller_id: &str) -> Vec<KgEdge> {
    use std::collections::HashMap;

    let mut counts: HashMap<String, u32> = HashMap::new();

    fn visit(node: Node, src: &[u8], counts: &mut HashMap<String, u32>) {
        match node.kind() {
            // Stop at nested function-like bodies so each method only
            // attributes its own direct calls.
            "method" | "singleton_method" | "class" | "module" | "do_block" | "block"
            | "lambda" => {
                return;
            }
            "call" => {
                if let Some(callee) = callee_name(node, src) {
                    if !is_declarative_call(&callee) && callee != "self" {
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
            to: build_call_target("ruby", "Method", &callee),
            kind: KgEdgeKind::Calls,
            weight: count as f32,
        })
        .collect()
}

/// Extract a best-effort callee name from a Ruby `call` node.
///
/// Why: Cross-file resolution is out of scope for the adapter (the linker
/// merges by qualified_name later). We only need a stable string handle.
/// What: Inspects the `method` field. Returns the bare text for `identifier`,
/// `constant`, or `operator`; returns `None` for unsupported forms.
/// Test: Exercised indirectly by `ruby_adapter_extracts_call_edges`.
fn callee_name(call: Node, src: &[u8]) -> Option<String> {
    let m = call.child_by_field_name("method")?;
    match m.kind() {
        "identifier" | "constant" | "operator" => Some(node_text(m, src)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(content: &str) -> CodeChunk {
        CodeChunk {
            id: "f.rb:1:10".into(),
            file: "f.rb".into(),
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
    fn ruby_supports_rb_files() {
        let a = RubyAnalyzer::new();
        assert!(a.supports("foo.rb"));
        assert!(a.supports("Rakefile.rb"));
        assert!(!a.supports("foo.py"));
        assert!(!a.supports("foo.rs"));
    }

    #[test]
    fn ruby_extracts_class_methods_with_qualified_ids() {
        let a = RubyAnalyzer::new();
        let src = "class Foo\n  def bar\n  end\n  def baz\n  end\nend\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(methods.len(), 2, "expected two methods, got {methods:?}");
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
    fn ruby_singleton_method_uses_class_qualified_name() {
        let a = RubyAnalyzer::new();
        let src = "class Greeter\n  def self.hello\n  end\nend\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(methods.len(), 1, "graph: {:?}", r.graph.nodes);
        let m = methods[0];
        assert_eq!(m.name, "hello");
        assert_eq!(
            m.qualified_name, "Greeter.hello",
            "qualified name should be Class.method, got {}",
            m.qualified_name
        );
        assert!(
            m.id.contains(":Greeter:hello"),
            "id should embed Greeter class, got {}",
            m.id
        );
    }

    #[test]
    fn ruby_module_emits_interface_node() {
        let a = RubyAnalyzer::new();
        let src = "module Util\n  def helper\n  end\nend\n";
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
            "expected one Interface node, got {interfaces:?}"
        );
        assert_eq!(interfaces[0].name, "Util");
    }

    #[test]
    fn ruby_class_emits_class_node() {
        let a = RubyAnalyzer::new();
        let src = "class Foo\nend\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        assert!(r
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Foo"));
    }

    #[test]
    fn ruby_adapter_extracts_call_edges() {
        let a = RubyAnalyzer::new();
        let src = "class Worker\n  def run\n    helper()\n    other.method()\n  end\nend\n";
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
        let has_method = calls.iter().any(|e| e.to.ends_with(":method"));
        assert!(has_helper, "expected edge to 'helper', got {calls:?}");
        assert!(has_method, "expected edge to 'method', got {calls:?}");
        assert!(
            calls
                .iter()
                .all(|e| e.from.contains(":Method:") && e.from.contains(":Worker:run")),
            "call edges should originate from Worker.run, got {calls:?}"
        );
    }

    #[test]
    fn ruby_adapter_deduplicates_repeated_calls() {
        let a = RubyAnalyzer::new();
        let src = "class Foo\n  def caller\n    bar()\n    bar()\n    bar()\n  end\nend\n";
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
    fn ruby_extracts_require_imports() {
        let a = RubyAnalyzer::new();
        let src = "require 'ostruct'\nrequire_relative 'helper'\n";
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
            names.contains(&"ostruct"),
            "expected 'ostruct' import target, got {names:?}"
        );
        assert!(
            names.contains(&"helper"),
            "expected 'helper' import target, got {names:?}"
        );
        // Imports edges from file
        let import_edges: Vec<&KgEdge> = r
            .graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Imports))
            .collect();
        assert_eq!(import_edges.len(), 2);
        assert!(import_edges.iter().all(|e| e.from == "ruby:File:f.rb"));
    }

    #[test]
    fn ruby_top_level_method_emits_method_node() {
        let a = RubyAnalyzer::new();
        let src = "def hello\nend\n";
        let r = a.analyze_chunks(&[make_chunk(src)]);
        let methods: Vec<&KgNode> = r
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, KgNodeKind::Method))
            .collect();
        assert_eq!(methods.len(), 1, "graph: {:?}", r.graph.nodes);
        assert_eq!(methods[0].name, "hello");
        assert_eq!(methods[0].qualified_name, "hello");
    }
}

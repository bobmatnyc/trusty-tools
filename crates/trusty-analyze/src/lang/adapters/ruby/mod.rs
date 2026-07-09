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

use crate::lang::{LanguageAnalyzer, StaticAnalysisResult};
use crate::types::{CodeChunk, KgEdge, KgEdgeKind, KgGraph, KgNodeKind};
use tree_sitter::{Node, Parser};

mod calls;
mod nodes;

use calls::{emit_require, extract_calls, is_require_call};
use nodes::{file_node, make_method_node, make_simple_node, name_of, node_text};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::KgNode;

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

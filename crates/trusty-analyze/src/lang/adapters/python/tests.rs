//! Unit tests for the Python adapter.
//!
//! Why: coverage lives in a sibling test module so `mod.rs` stays under the
//! 500-SLOC production cap (see #1195); test files carry the 1500-SLOC cap.
//! What: parses representative snippets and asserts the emitted nodes/edges.
//! Test: this *is* the test module.

use super::*;
use crate::types::{KgNode, KgNodeKind};

fn make_chunk(content: &str) -> CodeChunk {
    CodeChunk {
        id: "f.py:1:10".into(),
        file: "f.py".into(),
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
fn python_supports_py_files() {
    let a = PythonAnalyzer::new();
    assert!(a.supports("foo.py"));
    assert!(a.supports("stubs.pyi"));
    assert!(!a.supports("foo.rs"));
}

#[test]
fn python_extracts_function() {
    let a = PythonAnalyzer::new();
    let c = make_chunk("def hello():\n    pass\n");
    let r = a.analyze_chunks(&[c]);
    assert_eq!(r.analyzed_chunks, 1);
    let funcs: Vec<&KgNode> = r
        .graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, KgNodeKind::Function))
        .collect();
    assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph.nodes);
    assert_eq!(funcs[0].name, "hello");
    assert_eq!(funcs[0].language, "python");
    assert!(funcs[0].is_public);
}

#[test]
fn python_extracts_class() {
    let a = PythonAnalyzer::new();
    let c = make_chunk("class Foo:\n    def bar(self):\n        pass\n");
    let r = a.analyze_chunks(&[c]);
    let kinds: Vec<&KgNodeKind> = r.graph.nodes.iter().map(|n| &n.kind).collect();
    assert!(
        kinds.iter().any(|k| matches!(k, KgNodeKind::Class)),
        "expected Class, got {:?}",
        kinds
    );
    assert!(
        kinds.iter().any(|k| matches!(k, KgNodeKind::Method)),
        "expected Method, got {:?}",
        kinds
    );
}

#[test]
fn python_private_function_is_not_public() {
    let a = PythonAnalyzer::new();
    let c = make_chunk("def _hidden():\n    pass\n");
    let r = a.analyze_chunks(&[c]);
    let f = r
        .graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, KgNodeKind::Function))
        .expect("function node");
    assert!(!f.is_public);
}

#[test]
fn python_test_function_detected() {
    let a = PythonAnalyzer::new();
    let c = make_chunk("def test_login():\n    pass\n");
    let r = a.analyze_chunks(&[c]);
    assert!(r
        .graph
        .nodes
        .iter()
        .any(|n| matches!(n.kind, KgNodeKind::TestCase)));
}

#[test]
fn python_extracts_imports() {
    let a = PythonAnalyzer::new();
    let c = make_chunk("import os\nfrom pathlib import Path\n");
    let r = a.analyze_chunks(&[c]);
    let imports: Vec<&KgNode> = r
        .graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, KgNodeKind::Import))
        .collect();
    assert_eq!(imports.len(), 2, "graph: {:?}", r.graph.nodes);
    let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.contains(&"os"),
        "expected 'os' import target, got {names:?}"
    );
    assert!(
        names.contains(&"pathlib.Path"),
        "expected 'pathlib.Path' import target, got {names:?}"
    );
}

#[test]
fn python_extracts_class_methods_with_qualified_ids() {
    let a = PythonAnalyzer::new();
    let c = make_chunk(
        "class Foo:\n    def bar(self):\n        pass\n    def baz(self):\n        pass\n",
    );
    let r = a.analyze_chunks(&[c]);
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
}

#[test]
fn python_adapter_extracts_call_edges() {
    let a = PythonAnalyzer::new();
    let src = "def caller():\n    helper()\n    obj.method()\n\ndef helper():\n    pass\n";
    let c = make_chunk(src);
    let r = a.analyze_chunks(&[c]);
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
    let has_helper = calls.iter().any(|e| e.to.contains("helper"));
    let has_method = calls.iter().any(|e| e.to.contains("method"));
    assert!(has_helper, "expected edge to 'helper', got {calls:?}");
    assert!(has_method, "expected edge to 'method', got {calls:?}");
    assert!(
        calls
            .iter()
            .all(|e| e.from.contains(":Function:") || e.from.contains(":Method:")),
        "Calls edges should originate from a function/method node, got {calls:?}"
    );
}

#[test]
fn python_adapter_deduplicates_repeated_calls() {
    let a = PythonAnalyzer::new();
    let src = "def caller():\n    foo()\n    foo()\n    foo()\n\ndef foo():\n    pass\n";
    let c = make_chunk(src);
    let r = a.analyze_chunks(&[c]);
    let foo_edges: Vec<&KgEdge> = r
        .graph
        .edges
        .iter()
        .filter(|e| matches!(e.kind, KgEdgeKind::Calls) && e.to.contains("foo"))
        .collect();
    assert_eq!(
        foo_edges.len(),
        1,
        "repeated calls should be deduplicated, got {foo_edges:?}"
    );
    assert!(
        (foo_edges[0].weight - 3.0).abs() < f32::EPSILON,
        "weight should reflect call count=3, got {}",
        foo_edges[0].weight
    );
}

#[test]
fn python_method_call_edges_scoped_to_method() {
    let a = PythonAnalyzer::new();
    let src = "class Foo:\n    def bar(self):\n        helper()\n        helper()\n\ndef helper():\n    pass\n";
    let c = make_chunk(src);
    let r = a.analyze_chunks(&[c]);
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

//! Unit tests for the PHP adapter.
//!
//! Why: coverage lives in a sibling test module so `mod.rs` stays under the
//! 500-SLOC production cap (see #1195); test files carry the 1500-SLOC cap.
//! What: parses representative snippets and asserts the emitted nodes/edges.
//! Test: this *is* the test module.

use super::*;
use crate::types::KgNode;

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

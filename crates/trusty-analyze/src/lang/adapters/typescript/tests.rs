//! Unit tests for the TypeScript/JSX adapter.
//!
//! Why: coverage lives in a sibling test module so `mod.rs` stays under the
//! 500-SLOC production cap (see #1195); test files carry the 1500-SLOC cap.
//! What: parses representative snippets and asserts the emitted nodes/edges.
//! Test: this *is* the test module.

use super::*;
use crate::types::{KgNode, KgNodeKind};

fn make_chunk(content: &str, file: &str) -> CodeChunk {
    CodeChunk {
        id: format!("{file}:1:10"),
        file: file.into(),
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
fn ts_analyzer_extracts_function() {
    let a = TypeScriptAnalyzer::new();
    let c = make_chunk("function hello() { return 1; }\n", "f.ts");
    let r = a.analyze_chunks(&[c]);
    assert_eq!(r.analyzed_chunks, 1);
    let funcs: Vec<&KgNode> = r
        .graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, KgNodeKind::Function))
        .collect();
    assert_eq!(funcs.len(), 1);
    assert_eq!(funcs[0].name, "hello");
    assert_eq!(funcs[0].language, "typescript");
}

#[test]
fn ts_analyzer_extracts_class_and_interface() {
    let a = TypeScriptAnalyzer::new();
    let c = make_chunk(
        "interface Foo { x: number }\n\
             class Bar implements Foo { x = 1; }\n",
        "f.ts",
    );
    let r = a.analyze_chunks(&[c]);
    assert!(r
        .graph
        .nodes
        .iter()
        .any(|n| matches!(n.kind, KgNodeKind::Class) && n.name == "Bar"));
    assert!(r
        .graph
        .nodes
        .iter()
        .any(|n| matches!(n.kind, KgNodeKind::Interface) && n.name == "Foo"));
    assert!(r
        .graph
        .edges
        .iter()
        .any(|e| matches!(e.kind, KgEdgeKind::Implements)));
}

#[test]
fn supports_dot_ts_and_tsx() {
    let a = TypeScriptAnalyzer::new();
    assert!(a.supports("App.tsx"));
    assert!(a.supports("foo.ts"));
    assert!(!a.supports("foo.js"));
}

#[test]
fn ts_adapter_extracts_arrow_function_assigned_to_variable() {
    let a = TypeScriptAnalyzer::new();
    let c = make_chunk("const greet = (n: string) => `hi ${n}`;\n", "f.ts");
    let r = a.analyze_chunks(&[c]);
    let funcs: Vec<&KgNode> = r
        .graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, KgNodeKind::Function) && n.name == "greet")
        .collect();
    assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph);
    assert_eq!(funcs[0].language, "typescript");
}

#[test]
fn ts_adapter_extracts_call_edges() {
    let src = "function caller() {\n    helper();\n    obj.method();\n}\nfunction helper() {}\n";
    let c = make_chunk(src, "test.ts");
    let r = TypeScriptAnalyzer::new().analyze_chunks(&[c]);
    let calls: Vec<_> = r
        .graph
        .edges
        .iter()
        .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
        .collect();
    assert!(
        !calls.is_empty(),
        "expected at least one Calls edge, got none"
    );
    let has_helper = calls.iter().any(|e| e.to.contains("helper"));
    let has_method = calls.iter().any(|e| e.to.contains("method"));
    assert!(has_helper, "expected edge to 'helper', got {calls:?}");
    assert!(has_method, "expected edge to 'method', got {calls:?}");
    // Caller must be scoped to the function, not the file.
    assert!(
        calls
            .iter()
            .all(|e| e.from.contains(":Function:") || e.from.contains(":Method:")),
        "Calls edges should originate from a function/method node, got {calls:?}"
    );
}

#[test]
fn walk_declarations_emits_method_node_via_class_body() {
    // Methods are handled by walk_declarations when the recursion descends
    // into the class body. Hitting this path validates the dispatcher
    // routes `method_definition` correctly after refactoring.
    let src = "class Greeter { hello() { return 1; } }\n";
    let c = make_chunk(src, "g.ts");
    let r = TypeScriptAnalyzer::new().analyze_chunks(&[c]);
    let methods: Vec<&KgNode> = r
        .graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, KgNodeKind::Method) && n.name == "hello")
        .collect();
    assert_eq!(
        methods.len(),
        1,
        "expected one Method node, got: {:?}",
        r.graph.nodes
    );
}

#[test]
fn walk_imports_emits_import_and_export_nodes() {
    // Validates that walk_imports (the extracted sub-walker) produces both
    // Import and Export nodes after refactoring — same input, same output.
    let src = "import { x } from 'mod';\nexport const y = 1;\n";
    let c = make_chunk(src, "f.ts");
    let r = TypeScriptAnalyzer::new().analyze_chunks(&[c]);
    let has_import = r
        .graph
        .nodes
        .iter()
        .any(|n| matches!(n.kind, KgNodeKind::Import));
    let has_export = r
        .graph
        .nodes
        .iter()
        .any(|n| matches!(n.kind, KgNodeKind::Export));
    assert!(has_import, "expected Import node, got: {:?}", r.graph.nodes);
    assert!(has_export, "expected Export node, got: {:?}", r.graph.nodes);
    assert!(r
        .graph
        .edges
        .iter()
        .any(|e| matches!(e.kind, KgEdgeKind::Imports)));
    assert!(r
        .graph
        .edges
        .iter()
        .any(|e| matches!(e.kind, KgEdgeKind::Exports)));
}

#[test]
fn ts_adapter_deduplicates_repeated_calls() {
    let src = "function foo() {\n    bar();\n    bar();\n    bar();\n}\nfunction bar() {}\n";
    let c = make_chunk(src, "test.ts");
    let r = TypeScriptAnalyzer::new().analyze_chunks(&[c]);
    let bar_edges: Vec<_> = r
        .graph
        .edges
        .iter()
        .filter(|e| matches!(e.kind, KgEdgeKind::Calls) && e.to.contains("bar"))
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

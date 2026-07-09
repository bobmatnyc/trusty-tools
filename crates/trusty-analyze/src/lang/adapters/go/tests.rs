//! Unit tests for the Go adapter.
//!
//! Why: coverage lives in a sibling test module so `mod.rs` stays under the
//! 500-SLOC production cap (see #1195); test files carry the 1500-SLOC cap.
//! What: parses representative Go snippets and asserts the emitted
//! nodes/edges.
//! Test: this *is* the test module.

use super::*;
use crate::types::KgNode;

fn make_chunk(content: &str) -> CodeChunk {
    CodeChunk {
        id: "main.go:1:20".into(),
        file: "main.go".into(),
        start_line: 1,
        end_line: 20,
        content: content.into(),
        function_name: None,
        score: 0.0,
        compact_snippet: None,
        match_reason: String::new(),
    }
}

#[test]
fn go_supports_go_files() {
    let a = GoAnalyzer::new();
    assert!(a.supports("main.go"));
    assert!(!a.supports("main.rs"));
}

#[test]
fn go_extracts_function() {
    let a = GoAnalyzer::new();
    let c = make_chunk("package main\n\nfunc Hello() {}\n");
    let r = a.analyze_chunks(&[c]);
    assert_eq!(r.analyzed_chunks, 1);
    let funcs: Vec<&KgNode> = r
        .graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, KgNodeKind::Function))
        .collect();
    assert_eq!(funcs.len(), 1, "graph: {:?}", r.graph.nodes);
    assert_eq!(funcs[0].name, "Hello");
    assert!(funcs[0].is_public, "Hello should be exported");
}

#[test]
fn go_lowercase_function_is_not_public() {
    let a = GoAnalyzer::new();
    let c = make_chunk("package main\n\nfunc helper() {}\n");
    let r = a.analyze_chunks(&[c]);
    let f = r
        .graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, KgNodeKind::Function))
        .unwrap();
    assert!(!f.is_public);
}

#[test]
fn go_test_function_detected() {
    let a = GoAnalyzer::new();
    let c = make_chunk("package main\n\nimport \"testing\"\n\nfunc TestFoo(t *testing.T) {}\n");
    let r = a.analyze_chunks(&[c]);
    assert!(
        r.graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, KgNodeKind::TestCase) && n.name == "TestFoo"),
        "graph: {:?}",
        r.graph.nodes
    );
}

#[test]
fn go_extracts_struct_and_interface() {
    let a = GoAnalyzer::new();
    let c = make_chunk(
        "package main\n\
         \n\
         type Foo struct { X int }\n\
         type Bar interface { Run() }\n",
    );
    let r = a.analyze_chunks(&[c]);
    let kinds: Vec<&KgNodeKind> = r.graph.nodes.iter().map(|n| &n.kind).collect();
    assert!(kinds.iter().any(|k| matches!(k, KgNodeKind::Class)));
    assert!(kinds.iter().any(|k| matches!(k, KgNodeKind::Interface)));
}

#[test]
fn go_extracts_struct_class() {
    let a = GoAnalyzer::new();
    let c = make_chunk(
        "package main\n\
         \n\
         type Widget struct { N int }\n",
    );
    let r = a.analyze_chunks(&[c]);
    let class = r
        .graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, KgNodeKind::Class))
        .expect("expected a Class node for struct Widget");
    assert_eq!(class.name, "Widget");
    assert!(class.is_public);
}

#[test]
fn go_extracts_method() {
    let a = GoAnalyzer::new();
    let c = make_chunk(
        "package main\n\
         \n\
         type Foo struct{}\n\
         func (f *Foo) Bar() {}\n",
    );
    let r = a.analyze_chunks(&[c]);
    let method = r
        .graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, KgNodeKind::Method) && n.name == "Bar")
        .expect("expected Method node Bar");
    // Receiver type must be encoded into the ID and qualified_name.
    assert!(
        method.id.contains(":Foo:Bar"),
        "method id should embed receiver type, got {}",
        method.id
    );
    assert_eq!(method.qualified_name, "Foo.Bar");
}

#[test]
fn go_extracts_imports() {
    let a = GoAnalyzer::new();
    let c = make_chunk("package main\n\nimport (\n    \"fmt\"\n    \"os\"\n)\n");
    let r = a.analyze_chunks(&[c]);
    let imports: Vec<&KgNode> = r
        .graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, KgNodeKind::Import))
        .collect();
    assert_eq!(imports.len(), 2);
    // Quotes must be stripped from the import path.
    let names: Vec<&str> = imports.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"fmt"), "expected unquoted fmt: {names:?}");
    assert!(names.contains(&"os"), "expected unquoted os: {names:?}");
}

#[test]
fn go_extracts_single_import() {
    let a = GoAnalyzer::new();
    let c = make_chunk("package main\n\nimport \"fmt\"\n");
    let r = a.analyze_chunks(&[c]);
    let import_edges: Vec<&KgEdge> = r
        .graph
        .edges
        .iter()
        .filter(|e| matches!(e.kind, KgEdgeKind::Imports))
        .collect();
    assert_eq!(import_edges.len(), 1);
    assert!(
        import_edges[0].to.ends_with(":fmt"),
        "import edge target should end with :fmt, got {}",
        import_edges[0].to
    );
}

#[test]
fn go_doc_comment_captured() {
    let a = GoAnalyzer::new();
    let c = make_chunk("package main\n\n// Hello greets the world.\nfunc Hello() {}\n");
    let r = a.analyze_chunks(&[c]);
    let f = r
        .graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, KgNodeKind::Function))
        .unwrap();
    assert!(f.doc_comment.is_some());
    assert!(f.doc_comment.as_ref().unwrap().contains("greets"));
}

#[test]
fn go_adapter_extracts_call_edges() {
    let src = "package main\n\
               \n\
               func caller() {\n\
                   helper()\n\
                   fmt.Println(\"hi\")\n\
               }\n\
               \n\
               func helper() {}\n";
    let c = make_chunk(src);
    let r = GoAnalyzer::new().analyze_chunks(&[c]);
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
    let has_println = calls.iter().any(|e| e.to.contains("Println"));
    assert!(has_helper, "expected edge to 'helper', got {calls:?}");
    assert!(has_println, "expected edge to 'Println', got {calls:?}");
    // Caller must be scoped to the function/method, not the file.
    assert!(
        calls
            .iter()
            .all(|e| e.from.contains(":Function:") || e.from.contains(":Method:")),
        "Calls edges should originate from a function/method node, got {calls:?}"
    );
}

#[test]
fn go_adapter_deduplicates_repeated_calls() {
    let src = "package main\n\
               \n\
               func foo() {\n\
                   bar()\n\
                   bar()\n\
                   bar()\n\
               }\n\
               \n\
               func bar() {}\n";
    let c = make_chunk(src);
    let r = GoAnalyzer::new().analyze_chunks(&[c]);
    let bar_edges: Vec<&KgEdge> = r
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

#[test]
fn go_adapter_method_call_edges_scoped_to_method() {
    let src = "package main\n\
               \n\
               type Foo struct{}\n\
               \n\
               func (f *Foo) Bar() {\n\
                   helper()\n\
                   helper()\n\
               }\n\
               \n\
               func helper() {}\n";
    let c = make_chunk(src);
    let r = GoAnalyzer::new().analyze_chunks(&[c]);
    let calls: Vec<&KgEdge> = r
        .graph
        .edges
        .iter()
        .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
        .collect();
    assert_eq!(calls.len(), 1, "expected one deduped call edge: {calls:?}");
    assert!(
        calls[0].from.contains(":Method:") && calls[0].from.contains(":Foo:Bar"),
        "call edge should originate from method Foo.Bar, got {}",
        calls[0].from
    );
    assert!(
        (calls[0].weight - 2.0).abs() < f32::EPSILON,
        "weight should be 2, got {}",
        calls[0].weight
    );
}

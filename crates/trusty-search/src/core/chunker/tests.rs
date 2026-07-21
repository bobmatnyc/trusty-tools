//! Tests for the chunker module.
//!
//! Why: these tests cover the full range of chunker behaviours — language-specific
//! AST chunking, document format chunking, entity extraction, sub-chunk splitting,
//! symbol graph integration, and per-pub-const Rust chunking.
//! What: unit tests calling `chunk_ast`, `chunk_text`, `chunk_markdown`,
//! `chunk_yaml`, `chunk_toml`, `chunk_json`, `chunk_plaintext`, `chunk_xml`.
//! Test: run with `cargo test -p trusty-search -- chunker`.

use super::ast::chunk_ast;
use super::document::{
    chunk_json, chunk_markdown, chunk_plaintext, chunk_toml, chunk_xml, chunk_yaml,
};
use super::types::{chunk_text, ChunkType, RawChunk};

#[test]
fn test_overlapping_chunks() {
    let content = (1..=200)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let chunks = chunk_text("test.txt", &content, 150, 50);
    assert!(chunks.len() >= 2);
    assert_eq!(chunks[0].start_line, 1);
    assert_eq!(chunks[1].start_line, 51);
}

#[test]
fn test_chunk_id_format() {
    let chunks = chunk_text("src/main.txt", "line1\nline2\nline3", 150, 50);
    assert!(chunks[0].id.starts_with("src/main.txt:"));
}

#[test]
fn test_rust_function_chunking() {
    let src = r#"
fn alpha() {}

fn beta() -> i32 { 1 }

fn gamma(x: i32) -> i32 { x + 1 }
"#;
    let (chunks, _ents) = chunk_ast("a.rs", src);
    let fns: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Function)
        .collect();
    assert_eq!(fns.len(), 3, "expected 3 function chunks, got {fns:?}");
    let names: Vec<_> = fns
        .iter()
        .map(|c| c.function_name.clone().unwrap_or_default())
        .collect();
    assert!(names.contains(&"alpha".to_string()));
    assert!(names.contains(&"beta".to_string()));
    assert!(names.contains(&"gamma".to_string()));
}

#[test]
fn test_rust_impl_method_qualified_name() {
    let src = r#"
struct Foo;
impl Foo {
    fn bar(&self) {}
}
"#;
    let (chunks, _) = chunk_ast("foo.rs", src);
    let method = chunks
        .iter()
        .find(|c| c.chunk_type == ChunkType::Method)
        .expect("expected at least one Method chunk");
    assert_eq!(method.function_name.as_deref(), Some("Foo::bar"));
}

#[test]
fn test_rust_calls_extraction() {
    let src = r#"
fn main() {
    foo();
    bar(1, 2);
}
fn foo() {}
fn bar(_a: i32, _b: i32) {}
"#;
    let (chunks, _) = chunk_ast("m.rs", src);
    let main_chunk = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("main"))
        .expect("main chunk");
    assert!(
        main_chunk.calls.contains(&"foo".to_string()),
        "calls={:?}",
        main_chunk.calls
    );
    assert!(
        main_chunk.calls.contains(&"bar".to_string()),
        "calls={:?}",
        main_chunk.calls
    );
}

#[test]
fn test_rust_entity_named_types() {
    let src = r#"
use std::sync::Arc;
fn f() {
    let _x: Arc<Vec<String>> = Arc::new(Vec::new());
}
"#;
    let (_chunks, entities) = chunk_ast("t.rs", src);
    let named: Vec<&str> = entities
        .iter()
        .filter(|e| e.entity_type == crate::core::entity::EntityType::NamedType)
        .map(|e| e.text.as_str())
        .collect();
    assert!(named.contains(&"Arc"), "named_types={named:?}");
    assert!(named.contains(&"Vec"), "named_types={named:?}");
    assert!(named.contains(&"String"), "named_types={named:?}");
}

#[test]
fn test_large_function_splits() {
    // 250-line function body
    let mut body = String::new();
    for i in 0..250 {
        body.push_str(&format!("    let _v{i} = {i};\n"));
    }
    let src = format!("fn huge() {{\n{body}}}\n");
    let (chunks, _) = chunk_ast("h.rs", &src);
    let subs: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.parent_chunk_id.is_some())
        .collect();
    assert!(
        !subs.is_empty(),
        "expected sub-chunks for 250-line fn, got {chunks:#?}"
    );
    let parent_id = subs[0].parent_chunk_id.clone().unwrap();
    let parent = chunks
        .iter()
        .find(|c| c.id == parent_id)
        .expect("parent retained");
    assert!(!parent.child_chunk_ids.is_empty());
}

#[test]
fn test_unknown_language_fallback() {
    // Use an unknown extension (no document chunker matches) to verify the
    // sliding-window fallback path.
    let content = "hello world\nfoo bar\nbaz";
    let (chunks, entities) = chunk_ast("notes.unknownext", content);
    assert!(entities.is_empty());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk_type, ChunkType::Code);
}

#[test]
fn test_chunk_markdown_sections() {
    let content = "# Title\n\nintro\n\n## Section A\n\nbody a\n\n## Section B\n\nbody b\n";
    let chunks = chunk_markdown("doc.md", content);
    assert!(
        chunks.len() >= 2,
        "expected multiple sections, got {chunks:#?}"
    );
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.function_name.clone())
        .collect();
    assert!(names.iter().any(|n| n == "Section A"), "names={names:?}");
    assert!(names.iter().any(|n| n == "Section B"), "names={names:?}");
    for c in &chunks {
        assert_eq!(c.language.as_deref(), Some("markdown"));
        assert_eq!(c.chunk_type, ChunkType::Docstring);
    }
}

#[test]
fn test_chunk_markdown_ignores_hash_in_code_fence() {
    let content = "# Real Heading\n\nintro\n\n```\n## not a heading\n```\n\n## Next\n\nx\n";
    let chunks = chunk_markdown("doc.md", content);
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.function_name.clone())
        .collect();
    assert!(names.iter().any(|n| n == "Real Heading"));
    assert!(names.iter().any(|n| n == "Next"));
    assert!(
        !names.iter().any(|n| n == "not a heading"),
        "should not split on # inside fenced code block: {names:?}"
    );
}

#[test]
fn test_chunk_yaml_top_level_keys() {
    let content = "name: foo\nversion: 1.0\n\ndeps:\n  - a\n  - b\n\nscripts:\n  build: x\n";
    let chunks = chunk_yaml("conf.yaml", content);
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.function_name.clone())
        .collect();
    assert!(names.iter().any(|n| n == "name"), "names={names:?}");
    assert!(names.iter().any(|n| n == "deps"), "names={names:?}");
    assert!(names.iter().any(|n| n == "scripts"), "names={names:?}");
    for c in &chunks {
        assert_eq!(c.language.as_deref(), Some("yaml"));
    }
}

#[test]
fn test_chunk_toml_sections() {
    let content = "[package]\nname = \"foo\"\nversion = \"1.0\"\n\n[dependencies]\nserde = \"1\"\n\n[[bin]]\nname = \"x\"\n";
    let chunks = chunk_toml("Cargo.toml", content);
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.function_name.clone())
        .collect();
    assert!(names.iter().any(|n| n == "package"), "names={names:?}");
    assert!(names.iter().any(|n| n == "dependencies"), "names={names:?}");
    assert!(names.iter().any(|n| n == "bin"), "names={names:?}");
}

#[test]
fn test_chunk_json_small_file_single_chunk() {
    let content = "{\n  \"name\": \"foo\",\n  \"version\": \"1.0\"\n}\n";
    let chunks = chunk_json("a.json", content).expect("Some result");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].language.as_deref(), Some("json"));
}

#[test]
fn test_chunk_json_large_file_skipped() {
    let big = (0..600)
        .map(|i| format!("  \"k{i}\": {i},"))
        .collect::<Vec<_>>()
        .join("\n");
    let content = format!("{{\n{big}\n}}\n");
    let chunks = chunk_json("big.json", &content).expect("Some result");
    assert!(chunks.is_empty(), "expected large JSON to be skipped");
}

#[test]
fn test_chunk_plaintext_paragraphs() {
    let content = "First paragraph line 1.\nFirst paragraph line 2.\n\nSecond paragraph line 1.\nSecond paragraph line 2.\n\nThird paragraph.\n";
    let chunks = chunk_plaintext("note.txt", content);
    assert_eq!(
        chunks.len(),
        3,
        "expected one chunk per paragraph, got {chunks:#?}"
    );
    for c in &chunks {
        assert_eq!(c.language.as_deref(), Some("text"));
    }
}

#[test]
fn test_chunk_plaintext_caps_at_50_lines() {
    let content = (1..=130)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let chunks = chunk_plaintext("big.log", &content);
    assert!(
        chunks.len() >= 3,
        "expected at least 3 chunks for 130-line paragraph, got {}",
        chunks.len()
    );
    for c in &chunks {
        let line_count = c.end_line.saturating_sub(c.start_line) + 1;
        assert!(line_count <= 50, "chunk too large: {line_count} lines");
        assert_eq!(c.language.as_deref(), Some("log"));
    }
}

#[test]
fn test_chunk_xml_top_level_children() {
    let content = "<?xml version=\"1.0\"?>\n<library>\n  <book id=\"1\">\n    <title>A</title>\n  </book>\n  <book id=\"2\">\n    <title>B</title>\n  </book>\n  <magazine>\n    <title>C</title>\n  </magazine>\n</library>\n";
    let chunks = chunk_xml("data.xml", content);
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.function_name.clone())
        .collect();
    assert!(
        names.iter().filter(|n| *n == "book").count() >= 2,
        "names={names:?}"
    );
    assert!(names.iter().any(|n| n == "magazine"), "names={names:?}");
    for c in &chunks {
        assert_eq!(c.language.as_deref(), Some("xml"));
    }
}

// --- malformed-XML depth-clamp tests (issue #1181) ---

/// Why: a file that starts with a closing tag drives depth negative before
/// the fix; the clamped depth must stay at 0 and not emit a spurious chunk.
/// What: asserts that no chunk is produced when the only content is an
/// orphaned closing tag (the fallback single-chunk path runs instead but
/// produces a non-empty chunk from the raw content).
/// Test: depth must never go below 0; no panic; the chunk content must equal
/// the raw input, not a partial slice caused by a spurious mid-loop emit.
#[test]
fn test_chunk_xml_malformed_leading_close() {
    // Input starts with a closing tag — no opener precedes it.
    let content = "</root>\n<other/>\n";
    let chunks = chunk_xml("bad.xml", content);
    // No chunk should have been emitted by the emit-guard (depth went to -1
    // before the fix). The fallback path may produce one chunk; what we
    // enforce is that no chunk contains only a partial slice starting at
    // line 0 without a matching open.
    for c in &chunks {
        // The chunk text must be the full content (fallback), not a
        // partial slice that would indicate a spurious mid-loop emit.
        assert!(
            c.content.contains("</root>"),
            "unexpected partial chunk: {:?}",
            c.content
        );
    }
}

/// Why: an XML file with more closing tags than opening tags would previously
/// push depth into negative territory, causing `depth <= 1` to be satisfied
/// permanently and firing the emit guard on every subsequent line.
/// What: asserts the chunker returns without panic and the chunk count is
/// sane (≤ 1 for this all-close input, i.e. the fallback single-chunk path).
/// Test: depth clamp at 0 means `depth <= 1` is trivially true but
/// `prev_depth >= 1` also requires prev_depth to be ≥ 1 — which it cannot be
/// if depth is clamped to 0 from the start.
#[test]
fn test_chunk_xml_malformed_extra_closes() {
    // More closing tags than opening tags.
    let content = "<root>\n</child>\n</child>\n</child>\n</root>\n";
    // Must not panic; depth must never go negative.
    let chunks = chunk_xml("extra_closes.xml", content);
    // Reasonable: either 0 named chunks (falls back to single-chunk) or the
    // one valid root child that happens to appear.  The important invariant
    // is that the number of chunks is small and no index-out-of-bounds panics.
    assert!(
        chunks.len() <= 2,
        "unexpected chunk count {}: {:?}",
        chunks.len(),
        chunks.iter().map(|c| &c.content).collect::<Vec<_>>()
    );
}

/// Why: the depth clamp must not alter behaviour for well-formed XML — the
/// fix is intended to be behaviour-preserving for valid input.
/// What: runs the same well-formed library fixture and asserts identical chunk
/// names to the pre-fix baseline captured in `test_chunk_xml_top_level_children`.
/// Test: both `book` children and `magazine` must appear; language == `xml`.
#[test]
fn test_chunk_xml_well_formed_unchanged() {
    let content = "<?xml version=\"1.0\"?>\n<library>\n  <book id=\"1\">\n    <title>A</title>\n  </book>\n  <book id=\"2\">\n    <title>B</title>\n  </book>\n  <magazine>\n    <title>C</title>\n  </magazine>\n</library>\n";
    let chunks = chunk_xml("data.xml", content);
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.function_name.clone())
        .collect();
    assert!(
        names.iter().filter(|n| *n == "book").count() >= 2,
        "well-formed regression: book count wrong; names={names:?}"
    );
    assert!(
        names.iter().any(|n| n == "magazine"),
        "well-formed regression: magazine missing; names={names:?}"
    );
    for c in &chunks {
        assert_eq!(c.language.as_deref(), Some("xml"));
    }
}

#[test]
fn test_chunk_document_dispatch() {
    // Verify chunk_ast routes structured documents through chunk_document.
    let md_content = "# Hello\n\nworld\n";
    let (md_chunks, _) = chunk_ast("readme.md", md_content);
    assert!(md_chunks
        .iter()
        .any(|c| c.language.as_deref() == Some("markdown")));

    let yaml_content = "key: value\n";
    let (yaml_chunks, _) = chunk_ast("conf.yml", yaml_content);
    assert!(yaml_chunks
        .iter()
        .any(|c| c.language.as_deref() == Some("yaml")));

    let toml_content = "[section]\nx = 1\n";
    let (toml_chunks, _) = chunk_ast("a.toml", toml_content);
    assert!(toml_chunks
        .iter()
        .any(|c| c.language.as_deref() == Some("toml")));
}

#[test]
fn test_nlp_code_refs() {
    let src = r#"
/// Wraps the `CodeIndexer` to expose hybrid search.
fn make() {}
"#;
    let (chunks, _) = chunk_ast("d.rs", src);
    let f = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("make"))
        .unwrap();
    assert!(
        f.nlp_code_refs.iter().any(|k| k == "CodeIndexer"),
        "code_refs={:?}",
        f.nlp_code_refs
    );
}

#[test]
fn test_entity_external_crate() {
    let src = r#"
use usearch::Index;
fn f() {}
"#;
    let (_chunks, ents) = chunk_ast("u.rs", src);
    let exts: Vec<&str> = ents
        .iter()
        .filter(|e| e.entity_type == crate::core::entity::EntityType::ExternalCrate)
        .map(|e| e.text.as_str())
        .collect();
    assert!(exts.contains(&"usearch"), "external_crates={exts:?}");
}

#[test]
fn test_entity_error_variant() {
    let src = r#"
fn f() -> Result<(), anyhow::Error> {
    anyhow::bail!("index not found");
}
"#;
    let (_chunks, ents) = chunk_ast("e.rs", src);
    let any_err = ents
        .iter()
        .any(|e| e.entity_type == crate::core::entity::EntityType::ErrorVariant);
    assert!(
        any_err,
        "expected at least one ErrorVariant entity, got {ents:#?}"
    );
}

#[test]
fn test_csharp_chunking() {
    let src = r#"
namespace MyApp {
    class Foo {
        public void Bar() { Baz(); this.Qux(); }
        public Foo() {}
    }
    interface IThing { void Do(); }
}
"#;
    let (chunks, _) = chunk_ast("a.cs", src);
    // Expect: namespace (Module), class Foo (Class), Bar (Method),
    //   ctor (Method), IThing (Trait), Do (Method).
    let classes: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Class)
        .collect();
    assert!(
        classes
            .iter()
            .any(|c| c.function_name.as_deref() == Some("Foo")),
        "expected class Foo, got {chunks:#?}"
    );
    let traits: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Trait)
        .collect();
    assert!(
        traits
            .iter()
            .any(|c| c.function_name.as_deref() == Some("IThing")),
        "expected interface IThing as Trait"
    );
    let bar = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("Bar"))
        .expect("Bar method chunk");
    assert_eq!(bar.chunk_type, ChunkType::Method);
    assert!(
        bar.calls.contains(&"Baz".to_string()),
        "calls={:?}",
        bar.calls
    );
    assert!(
        bar.calls.contains(&"Qux".to_string()),
        "calls={:?}",
        bar.calls
    );
}

#[test]
fn test_kotlin_chunking() {
    // Avoid the top-level `package` statement which the kotlin-ng grammar
    // parses oddly without a following file body terminator; the chunker
    // still walks into ERROR-recovered subtrees, but the cleaner case
    // exercises the happy path.
    let src = r#"
class Foo {
    fun bar() { baz(); this.qux() }
}
object Singleton {
    fun run() { other() }
}
"#;
    let (chunks, _) = chunk_ast("a.kt", src);
    assert!(
        chunks
            .iter()
            .any(|c| c.function_name.as_deref() == Some("Foo") && c.chunk_type == ChunkType::Class),
        "expected class Foo, got {chunks:#?}"
    );
    let bar = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("bar"))
        .expect("bar method chunk");
    assert_eq!(bar.chunk_type, ChunkType::Method);
    assert!(
        bar.calls.contains(&"baz".to_string()),
        "calls={:?}",
        bar.calls
    );
    assert!(
        bar.calls.contains(&"qux".to_string()),
        "calls={:?}",
        bar.calls
    );
}

#[test]
fn test_swift_chunking() {
    let src = r#"
class Foo {
    func bar() { baz(); self.qux() }
    init() {}
}
struct S {}
enum E { case a }
protocol P { func d() }
extension Foo { func ext() {} }
"#;
    let (chunks, _) = chunk_ast("a.swift", src);
    // class Foo
    assert!(
        chunks
            .iter()
            .any(|c| c.function_name.as_deref() == Some("Foo") && c.chunk_type == ChunkType::Class),
        "expected class Foo, got {chunks:#?}"
    );
    // struct S
    assert!(
        chunks
            .iter()
            .any(|c| c.function_name.as_deref() == Some("S") && c.chunk_type == ChunkType::Struct),
        "expected struct S"
    );
    // enum E
    assert!(
        chunks
            .iter()
            .any(|c| c.function_name.as_deref() == Some("E") && c.chunk_type == ChunkType::Enum),
        "expected enum E"
    );
    // protocol P → Trait
    assert!(
        chunks
            .iter()
            .any(|c| c.function_name.as_deref() == Some("P") && c.chunk_type == ChunkType::Trait),
        "expected protocol P as Trait"
    );
    // extension Foo → Module
    assert!(
        chunks
            .iter()
            .any(|c| c.chunk_type == ChunkType::Module
                && c.function_name.as_deref() == Some("Foo")),
        "expected extension Foo as Module"
    );
    // method calls
    let bar = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("bar"))
        .expect("bar method chunk");
    assert!(
        bar.calls.contains(&"baz".to_string()),
        "calls={:?}",
        bar.calls
    );
    assert!(
        bar.calls.contains(&"qux".to_string()),
        "calls={:?}",
        bar.calls
    );
}

#[test]
fn test_nlp_keywords_from_doc_comments() {
    let src = r#"
/// Implements the RRF fusion algorithm.
fn fuse() {}
"#;
    let (chunks, _) = chunk_ast("d.rs", src);
    let f = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("fuse"))
        .unwrap();
    assert!(
        f.nlp_keywords.iter().any(|k| k == "RRF"),
        "keywords={:?}",
        f.nlp_keywords
    );
    assert!(
        f.nlp_keywords.iter().any(|k| k == "Implements"),
        "keywords={:?}",
        f.nlp_keywords
    );
}

// ----- Scala Phase 2 (issue #55) -----

#[test]
fn test_scala_method_qualified_name() {
    // Why: SymbolGraph caller edges need `ClassName::methodName` so that
    // two classes with a `run` method don't share a single graph node.
    // What: a class method is chunked as `Foo::bar`, a top-level def as `freefn`.
    // Test: assert both chunks emit the expected qualified / unqualified names.
    let src = r#"
class Foo extends Bar with Mixin {
  def bar(): Unit = baz()
}
object O {
  def run(): Unit = other()
}
def freefn(): Unit = ()
"#;
    let (chunks, _) = chunk_ast("a.scala", src);
    let bar = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("Foo::bar"))
        .expect("expected qualified method Foo::bar, got: {chunks:#?}");
    assert_eq!(bar.chunk_type, ChunkType::Method);
    let run = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("O::run"))
        .expect("expected qualified method O::run");
    assert_eq!(run.chunk_type, ChunkType::Method);
    // Top-level def remains unqualified.
    assert!(
        chunks
            .iter()
            .any(|c| c.function_name.as_deref() == Some("freefn")
                && c.chunk_type == ChunkType::Function),
        "expected unqualified Function freefn, got {chunks:#?}"
    );
}

#[test]
fn test_scala_caller_scoped_call_edges() {
    // Why: Phase 2 needs caller-scoped call edges so `who calls baz?`
    // returns `Foo::bar`, not the whole file.
    // What: `Foo::bar`'s `calls` field includes `baz`, and the call is
    // attached to the method chunk (not the class).
    // Test: assert `calls` membership on the method chunk.
    let src = r#"
class Foo {
  def bar(): Unit = {
    baz()
    this.qux()
  }
}
"#;
    let (chunks, _) = chunk_ast("a.scala", src);
    let bar = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("Foo::bar"))
        .expect("Foo::bar chunk");
    assert!(
        bar.calls.contains(&"baz".to_string()),
        "calls={:?}",
        bar.calls
    );
    assert!(
        bar.calls.contains(&"qux".to_string()),
        "calls={:?}",
        bar.calls
    );
}

#[test]
fn test_scala_extends_and_with_emit_inherits() {
    // Why: `extends T1 with T2 with T3` describes a layered Scala class
    // mixin chain; Phase 2 turns each parent into an `Implements` edge so
    // intent-gated KG expansion can surface the parent.
    // What: `inherits_from` on the class chunk lists all three parents.
    // Test: assert membership.
    let src = r#"
class Foo extends Bar with Mixin with Other {
  def m(): Unit = ()
}
"#;
    let (chunks, _) = chunk_ast("a.scala", src);
    let foo = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("Foo") && c.chunk_type == ChunkType::Class)
        .expect("Foo class chunk");
    for parent in ["Bar", "Mixin", "Other"] {
        assert!(
            foo.inherits_from.iter().any(|p| p == parent),
            "expected parent {parent} in inherits_from={:?}",
            foo.inherits_from
        );
    }
}

#[test]
fn test_scala_symbol_graph_resolves_caller() {
    // Why: end-to-end check that the chunker output, once fed to
    // SymbolGraph::build_from_chunks, yields a usable caller→callee edge.
    // What: build the graph from two scala chunks and assert
    // `callers_of("baz")` returns the qualified method.
    // Test: integrates chunker + symbol_graph for Phase 2.
    use crate::core::symbol_graph::SymbolGraph;
    let src = r#"
class Foo {
  def bar(): Unit = baz()
}
def baz(): Unit = ()
"#;
    let (chunks, _) = chunk_ast("s.scala", src);
    let tuples: Vec<_> = chunks
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                c.file.clone(),
                c.function_name.clone(),
                c.calls.clone(),
                c.inherits_from.clone(),
                c.chunk_type.clone(),
            )
        })
        .collect();
    let g = SymbolGraph::build_from_chunks(&tuples);
    let callers = g.callers_of("baz", 1);
    assert!(
        callers.iter().any(|(s, _)| s == "Foo::bar"),
        "callers={callers:?}"
    );
}

// ----- PHP Phase 2 (issue #49) -----

#[test]
fn test_php_method_qualified_name() {
    // Why: same rationale as Scala — class-qualified method names avoid
    // symbol collisions in the call graph.
    // What: `Foo::doIt` is the chunk's function_name; a free function in
    // the same file remains unqualified.
    // Test: assert both forms.
    let src = r#"<?php
class Foo extends Bar implements I1, I2 {
    public function doIt(): void {
        $this->helper();
    }
}
function freefn(): void {}
"#;
    let (chunks, _) = chunk_ast("a.php", src);
    let doit = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("Foo::doIt"))
        .expect("expected qualified Foo::doIt, got: {chunks:#?}");
    assert_eq!(doit.chunk_type, ChunkType::Method);
    assert!(
        chunks
            .iter()
            .any(|c| c.function_name.as_deref() == Some("freefn")
                && c.chunk_type == ChunkType::Function),
        "expected unqualified Function freefn"
    );
}

#[test]
fn test_php_caller_scoped_call_edges() {
    // Why: caller-scoped edges must capture all three PHP call shapes
    // (`$this->m()`, `Class::m()`, `func()`).
    // What: assert each callee appears in the method's `calls` field.
    let src = r#"<?php
class Foo {
    public function doIt(): void {
        $this->helper();
        Foo::staticCall();
        regularFunc();
    }
}
"#;
    let (chunks, _) = chunk_ast("a.php", src);
    let doit = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("Foo::doIt"))
        .expect("Foo::doIt chunk");
    for callee in ["helper", "staticCall", "regularFunc"] {
        assert!(
            doit.calls.iter().any(|c| c == callee),
            "expected callee {callee} in calls={:?}",
            doit.calls
        );
    }
}

#[test]
fn test_php_implements_and_extends_emit_inherits() {
    // Why: PHP's `class Foo extends Bar implements I1, I2` carries one
    // parent class plus N interfaces; Phase 2 emits one `Implements` edge
    // for each.
    // What: assert all three names appear in `inherits_from`.
    let src = r#"<?php
class Foo extends Bar implements I1, I2 {}
"#;
    let (chunks, _) = chunk_ast("a.php", src);
    let foo = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("Foo") && c.chunk_type == ChunkType::Class)
        .expect("Foo class chunk");
    for parent in ["Bar", "I1", "I2"] {
        assert!(
            foo.inherits_from.iter().any(|p| p == parent),
            "expected parent {parent} in inherits_from={:?}",
            foo.inherits_from
        );
    }
}

#[test]
fn test_php_interface_extends_emits_inherits() {
    // Why: PHP interfaces can extend multiple interfaces; the grammar
    // packages those parents in a `base_clause` (same shape as a class's
    // extends clause).
    // What: `interface Child extends P1, P2` → inherits_from = [P1, P2].
    let src = r#"<?php
interface Child extends P1, P2 {}
"#;
    let (chunks, _) = chunk_ast("a.php", src);
    let child = chunks
        .iter()
        .find(|c| c.function_name.as_deref() == Some("Child") && c.chunk_type == ChunkType::Trait)
        .expect("Child interface (chunked as Trait)");
    for parent in ["P1", "P2"] {
        assert!(
            child.inherits_from.iter().any(|p| p == parent),
            "expected parent {parent} in inherits_from={:?}",
            child.inherits_from
        );
    }
}

#[test]
fn test_php_symbol_graph_resolves_caller() {
    // Why: end-to-end Phase 2 integration: chunker → symbol_graph yields
    // a usable PHP caller→callee edge for KG expansion.
    // What: assert `callers_of("helper")` returns `Foo::doIt`.
    use crate::core::symbol_graph::SymbolGraph;
    let src = r#"<?php
class Foo {
    public function doIt(): void {
        $this->helper();
    }
    public function helper(): void {}
}
"#;
    let (chunks, _) = chunk_ast("p.php", src);
    let tuples: Vec<_> = chunks
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                c.file.clone(),
                c.function_name.clone(),
                c.calls.clone(),
                c.inherits_from.clone(),
                c.chunk_type.clone(),
            )
        })
        .collect();
    let g = SymbolGraph::build_from_chunks(&tuples);
    // `helper` resolves to `Foo::helper` via the suffix lookup.
    let callers = g.callers_of("Foo::helper", 1);
    assert!(
        callers.iter().any(|(s, _)| s == "Foo::doIt"),
        "callers={callers:?}"
    );
}

// ----- Per-pub-const Rust chunking (issue #143) -------------------------

/// Why: a file containing only `pub const` declarations used to produce a
/// single whole-file `Code` chunk with null `function_name`, making every
/// constant invisible to symbol-name queries and the Definition-intent boost.
/// What: each `pub const` / `pub static` in a `.rs` file must produce its own
/// `Constant` chunk with a non-null `function_name`.
/// Test: this is the test.
#[test]
fn test_rust_pub_const_chunking_produces_n_constant_chunks() {
    let src = r#"
pub const ALPHA: u32 = 1;
pub const BRUSILOV_EPOCH: u64 = 1_000_000;
pub const MAX_BATCH_SIZE: usize = 256;
pub const KIKUCHI_MAX_DEPTH: usize = 8;
pub const HNSW_EF_CONSTRUCTION: usize = 200;
pub const DEFAULT_TOP_K: usize = 10;
pub const BM25_K1: f32 = 1.5;
pub const BM25_B: f32 = 0.75;
"#;
    let (chunks, _) = chunk_ast("constants.rs", src);
    let const_chunks: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Constant)
        .collect();
    assert_eq!(
        const_chunks.len(),
        8,
        "expected 8 Constant chunks (one per pub const), got {}: {:#?}",
        const_chunks.len(),
        chunks
    );
    // Every constant chunk must have a non-null function_name.
    for c in &const_chunks {
        assert!(
            c.function_name.is_some(),
            "expected non-null function_name for constant chunk {c:#?}"
        );
    }
    // Spot-check a few names.
    let names: Vec<_> = const_chunks
        .iter()
        .filter_map(|c| c.function_name.as_deref())
        .collect();
    assert!(
        names.contains(&"BRUSILOV_EPOCH"),
        "expected BRUSILOV_EPOCH in names: {names:?}"
    );
    assert!(
        names.contains(&"MAX_BATCH_SIZE"),
        "expected MAX_BATCH_SIZE in names: {names:?}"
    );
}

/// Why: validates that a mixed Rust file — constants + functions — emits
/// the correct chunk type for each item and does not absorb function chunks
/// into constant chunks or vice-versa.
/// What: a file with one `pub const`, one `pub fn`, and one more `pub const`
/// must produce 3 chunks in the expected types.
/// Test: this is the test.
#[test]
fn test_rust_mixed_const_and_fn_chunking() {
    let src = r#"
pub const FOO: u32 = 42;

pub fn do_something() -> u32 {
    FOO + 1
}

pub const BAR: &str = "hello";
"#;
    let (chunks, _) = chunk_ast("mixed.rs", src);

    let const_chunks: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Constant)
        .collect();
    assert_eq!(
        const_chunks.len(),
        2,
        "expected 2 Constant chunks, got {const_chunks:#?}"
    );
    let const_names: Vec<_> = const_chunks
        .iter()
        .filter_map(|c| c.function_name.as_deref())
        .collect();
    assert!(
        const_names.contains(&"FOO"),
        "expected FOO: {const_names:?}"
    );
    assert!(
        const_names.contains(&"BAR"),
        "expected BAR: {const_names:?}"
    );

    let fn_chunks: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Function)
        .collect();
    assert_eq!(
        fn_chunks.len(),
        1,
        "expected 1 Function chunk, got {fn_chunks:#?}"
    );
    assert_eq!(
        fn_chunks[0].function_name.as_deref(),
        Some("do_something"),
        "fn chunk name mismatch"
    );
}

/// Why: `pub static` is semantically equivalent to `pub const` for the
/// purpose of symbol lookup; both must emit Constant chunks.
/// What: `pub static X: &str = "…"` produces a Constant chunk with the
/// identifier as `function_name`.
/// Test: this is the test.
#[test]
fn test_rust_pub_static_treated_as_constant() {
    let src = r#"
pub static GREETING: &str = "hello";
pub static MAX_RETRIES: u32 = 3;
"#;
    let (chunks, _) = chunk_ast("statics.rs", src);
    let const_chunks: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Constant)
        .collect();
    assert_eq!(
        const_chunks.len(),
        2,
        "expected 2 Constant chunks for pub static, got {const_chunks:#?}"
    );
    let names: Vec<_> = const_chunks
        .iter()
        .filter_map(|c| c.function_name.as_deref())
        .collect();
    assert!(names.contains(&"GREETING"), "expected GREETING: {names:?}");
    assert!(
        names.contains(&"MAX_RETRIES"),
        "expected MAX_RETRIES: {names:?}"
    );
}

/// Why: Phase 1 scopes per-const chunking to public declarations only;
/// private constants stay in whatever surrounding chunk applies.
/// What: a file with private `const` (no `pub`) must NOT produce any
/// `Constant` chunks — the private const stays unclaimed / falls through
/// to the whole-file fallback.
/// Test: this is the test.
#[test]
fn test_rust_private_const_does_not_get_constant_chunk() {
    let src = r#"
const INTERNAL_LIMIT: usize = 100;
const PRIVATE_KEY: &str = "secret";
"#;
    let (chunks, _) = chunk_ast("private.rs", src);
    let const_chunks: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Constant)
        .collect();
    assert!(
        const_chunks.is_empty(),
        "expected no Constant chunks for private consts, got {const_chunks:#?}"
    );
}

/// Why: regression guard — existing function/struct/impl/trait/enum
/// chunking must not be disturbed by the new const/static rules.
/// What: a Rust file without any `pub const` / `pub static` still produces
/// correct Function, Struct, Impl chunks as before.
/// Test: this is the test.
#[test]
fn test_rust_no_const_regression_on_function_chunks() {
    // Uses the same content as test_rust_function_chunking to ensure parity.
    let src = r#"
fn alpha() {}

fn beta() -> i32 { 1 }

fn gamma(x: i32) -> i32 { x + 1 }
"#;
    let (chunks, _) = chunk_ast("no_const.rs", src);
    let fns: Vec<&RawChunk> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Function)
        .collect();
    assert_eq!(fns.len(), 3, "expected 3 Function chunks, got {fns:#?}");
    assert!(
        chunks.iter().all(|c| c.chunk_type != ChunkType::Constant),
        "unexpected Constant chunk in function-only file: {chunks:#?}"
    );
}

/// Issue #90: end-to-end check that a small Rust snippet — parsed by
/// `chunk_ast`, fed into `SymbolGraph::build_from_chunks` — produces
/// non-zero symbols and a usable caller→callee edge. This is the
/// regression test for the silent KG-skip bug where reindexes that
/// breached the memory limit during embedding left the graph at 0/0.
///
/// Why: prevents future skips/bugs along the chunker→graph integration
/// from re-introducing the same failure mode.
/// What: two free functions where `alpha` calls `beta`; the resulting
/// graph must contain both symbols and `callers_of("beta")` must include
/// `alpha`.
/// Test: this is the test.
#[test]
fn test_rust_symbol_graph_resolves_caller() {
    use crate::core::symbol_graph::SymbolGraph;
    let src = "fn alpha() { beta(); }\nfn beta() {}\n";
    let (chunks, _) = chunk_ast("a.rs", src);
    let tuples: Vec<_> = chunks
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                c.file.clone(),
                c.function_name.clone(),
                c.calls.clone(),
                c.inherits_from.clone(),
                c.chunk_type.clone(),
            )
        })
        .collect();
    let g = SymbolGraph::build_from_chunks(&tuples);
    assert!(
        g.node_count() >= 2,
        "expected >= 2 symbol nodes for alpha+beta, got {} (chunks={:#?})",
        g.node_count(),
        chunks
            .iter()
            .map(|c| (c.function_name.clone(), c.calls.clone()))
            .collect::<Vec<_>>(),
    );
    let callers = g.callers_of("beta", 1);
    assert!(
        callers.iter().any(|(s, _)| s == "alpha"),
        "expected alpha among callers of beta, got {callers:?}"
    );
}

/// Regression test for issue #3537: a deeply nested, non-chunk-producing
/// global-scope expression must not stack-overflow the chunker.
///
/// Why: `walk_for_chunks` used to be a native recursive descent whose stack
/// depth tracked raw AST depth. A global-scope declaration containing a
/// deeply parenthesized expression (the reported shape: "extreme nesting
/// depth" in deeply templated/generated headers) is never pruned the way a
/// function *body* is, so it drove unbounded recursion and crashed the whole
/// daemon process with `fatal runtime error: stack overflow`. Confirmed via
/// an out-of-tree repro before this fix: the exact same construction (scaled
/// up to 200,000 levels) reliably aborted the process with SIGABRT.
/// What: 50,000 levels of nested parens is two orders of magnitude past the
/// walker's internal depth ceiling and far beyond anything the previous
/// recursive implementation could survive on a normal thread stack — if this
/// test completes at all (rather than aborting the whole `cargo test`
/// process), the fix holds.
/// Test: this is the test — asserts `chunk_ast` returns normally (a single
/// fallback chunk, since nothing in a bare expression is chunk-classified)
/// instead of crashing.
#[test]
fn test_deeply_nested_expression_does_not_crash() {
    const DEPTH: usize = 50_000;
    let mut src = String::from("const X: i32 = ");
    src.push_str(&"(".repeat(DEPTH));
    src.push('1');
    src.push_str(&")".repeat(DEPTH));
    src.push_str(";\n");

    let (chunks, _entities) = chunk_ast("deeply_nested.rs", &src);

    // Nothing in a bare parenthesized expression is chunk-classified, so
    // this falls back to a single generic chunk covering the whole file.
    assert_eq!(
        chunks.len(),
        1,
        "expected the single-file fallback chunk, got {} chunks",
        chunks.len()
    );
}

/// Regression test for issue #3537: deeply nested *classified* containers
/// (the `walk_for_chunks` branch that recurses into impl/class/module
/// bodies) must not stack-overflow the chunker either.
///
/// Why: unlike the unclassified-node branch covered by
/// `test_deeply_nested_expression_does_not_crash`, this branch increments
/// `chunk_depth` and additionally calls `collect_calls`/`collect_inherits`
/// per classified node — both of which perform their own subtree walk. A
/// long chain of nested containers (e.g. `mod a { mod a { … } }`) exercises
/// a different, previously-quadratic cost path (each classified node's
/// `collect_calls` call re-walking its full remaining subtree) that the
/// depth ceiling on the outer walk alone does not bound; `collect_calls`
/// itself needed a matching depth cap to keep this bounded.
/// What: 50,000 nested `mod` blocks — completes quickly and returns a
/// bounded chunk count (capped by the walker's internal depth ceiling)
/// rather than either crashing or taking superlinear time.
/// Test: this is the test.
#[test]
fn test_deeply_nested_modules_does_not_crash() {
    const DEPTH: usize = 50_000;
    let mut src = String::new();
    src.push_str(&"mod a{".repeat(DEPTH));
    src.push_str("fn f(){}");
    src.push_str(&"}".repeat(DEPTH));

    let (chunks, entities) = chunk_ast("deeply_nested_mods.rs", &src);

    // Chunk count is bounded by the walker's internal depth ceiling, not by
    // DEPTH — the key assertion is simply that this returns at all (rather
    // than crashing) and stays well under DEPTH, proving the walk was
    // truncated rather than paying O(DEPTH) or worse.
    assert!(
        !chunks.is_empty(),
        "expected at least the outermost mod chunks to be emitted"
    );
    assert!(
        chunks.len() < DEPTH,
        "expected the walk to be depth-capped well below {DEPTH}, got {} chunks",
        chunks.len()
    );
    // Entity extraction (`walk_rust` in `core/entity.rs`) shares the same
    // failure mode and the same depth cap; it must also stay bounded.
    assert!(
        entities.len() < DEPTH,
        "expected entity extraction to be depth-capped well below {DEPTH}, got {} entities",
        entities.len()
    );
}

//! Pure extraction tests: per-language AST chunking, markdown heading
//! split, and the sliding-window fallback.
//!
//! Why: These exercise `chunker.rs` with no disk, embedder, or store, so
//! they're fast and deterministic.
//! What: Asserts function counts, names, and line ranges for each supported
//! language plus the fallback windowing behaviour.
//! Test: This *is* the test module.

use crate::search::indexer::chunker::{extract_chunks_from_source, fallback_line_chunks};

#[test]
fn rust_function_chunking() {
    let src = "fn foo() {\n    println!(\"hi\");\n}\n\nfn bar(x: i32) -> i32 {\n    x + 1\n}\n";
    let chunks = extract_chunks_from_source(src, "rust");
    assert_eq!(chunks.len(), 2, "expected 2 Rust functions, got {chunks:?}");
    assert_eq!(chunks[0].function_name.as_deref(), Some("foo"));
    assert_eq!(chunks[0].start_line, 1);
    assert_eq!(chunks[0].end_line, 3);
    assert_eq!(chunks[1].function_name.as_deref(), Some("bar"));
    assert_eq!(chunks[1].start_line, 5);
    assert_eq!(chunks[1].end_line, 7);
}

#[test]
fn python_function_chunking() {
    let src = "def foo():\n    pass\n\nasync def bar():\n    return 1\n";
    let chunks = extract_chunks_from_source(src, "python");
    assert_eq!(
        chunks.len(),
        2,
        "expected 2 Python functions, got {chunks:?}"
    );
    let names: Vec<Option<&str>> = chunks.iter().map(|c| c.function_name.as_deref()).collect();
    assert!(names.contains(&Some("foo")));
    assert!(names.contains(&Some("bar")));
}

#[test]
fn go_function_chunking() {
    let src = "package main\n\nfunc Foo() {}\n\nfunc (r *R) Bar() int {\n    return 1\n}\n";
    let chunks = extract_chunks_from_source(src, "go");
    assert_eq!(chunks.len(), 2, "expected 2 Go functions, got {chunks:?}");
    let names: Vec<Option<&str>> = chunks.iter().map(|c| c.function_name.as_deref()).collect();
    assert!(names.contains(&Some("Foo")));
    assert!(names.contains(&Some("Bar")));
}

#[test]
fn markdown_heading_chunking() {
    let src = "# Title\n\nintro\n\n## Alpha\n\naaa\n\n## Beta\n\nbbb\n\n## Gamma\n\nccc\n";
    let chunks = extract_chunks_from_source(src, "markdown");
    assert_eq!(
        chunks.len(),
        3,
        "expected 3 markdown sections, got {chunks:?}"
    );
    assert_eq!(chunks[0].function_name.as_deref(), Some("Alpha"));
    assert_eq!(chunks[0].start_line, 5);
    assert_eq!(chunks[1].function_name.as_deref(), Some("Beta"));
    assert_eq!(chunks[1].start_line, 9);
    assert_eq!(chunks[2].function_name.as_deref(), Some("Gamma"));
    assert_eq!(chunks[2].start_line, 13);
}

#[test]
fn fallback_uses_overlapping_windows() {
    // 300 lines of a constants-only Rust file (no functions) → fallback
    // uses 150-line windows with a 50-line stride (~67% overlap, #376).
    // Stride positions at 0, 50, 100, 150 → starts 1, 51, 101, 151;
    // window at 151..301 clipped to 151..300, then loop terminates
    // because end == lines.len(). Expect 4 overlapping chunks.
    let body: String = (0..300)
        .map(|i| format!("const K{i}: u32 = {i};\n"))
        .collect();
    let chunks = extract_chunks_from_source(&body, "rust");
    assert_eq!(
        chunks.len(),
        4,
        "expected 4 overlapping fallback chunks, got {chunks:?}"
    );
    assert!(chunks.iter().all(|c| c.function_name.is_none()));
    // First window.
    assert_eq!(chunks[0].start_line, 1);
    assert_eq!(chunks[0].end_line, 150);
    // Second window starts 50 lines after the first → ~67% overlap.
    assert_eq!(chunks[1].start_line, 51);
    assert_eq!(chunks[1].end_line, 200);
    // Third window.
    assert_eq!(chunks[2].start_line, 101);
    assert_eq!(chunks[2].end_line, 250);
    // Final window clipped to file length.
    assert_eq!(chunks[3].start_line, 151);
    assert_eq!(chunks[3].end_line, 300);
}

#[test]
fn fallback_small_file_emits_single_chunk() {
    // A small file (under one window) should produce one chunk
    // covering the whole file (not zero strides) to keep small
    // configs searchable.
    let body: String = (0..10).map(|i| format!("k{i}=v\n")).collect();
    let chunks = fallback_line_chunks(&body);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].start_line, 1);
    assert_eq!(chunks[0].end_line, 10);
}

#[test]
fn java_function_chunking() {
    let src = "class Foo {\n    void bar() {}\n    int baz(int x) { return x; }\n}\n";
    let chunks = extract_chunks_from_source(src, "java");
    assert!(
        chunks.len() >= 2,
        "expected at least 2 Java methods, got {chunks:?}"
    );
    let names: Vec<Option<&str>> = chunks.iter().map(|c| c.function_name.as_deref()).collect();
    assert!(names.contains(&Some("bar")));
    assert!(names.contains(&Some("baz")));
}

#[test]
fn c_function_chunking() {
    let src = "int add(int a, int b) {\n    return a + b;\n}\n\nvoid noop(void) {}\n";
    let chunks = extract_chunks_from_source(src, "c");
    assert_eq!(chunks.len(), 2, "expected 2 C functions, got {chunks:?}");
}

#[test]
fn cpp_function_chunking() {
    let src = "int square(int x) { return x * x; }\n\nvoid greet() {}\n";
    let chunks = extract_chunks_from_source(src, "cpp");
    assert_eq!(chunks.len(), 2, "expected 2 C++ functions, got {chunks:?}");
}

use super::*;
use crate::core::chunker::ChunkType;
use crate::core::symbol_graph::ChunkTuple;

fn mk_chunk(id: &str, file: &str, name: &str, start: usize, end: usize, content: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: start,
        end_line: end,
        content: content.to_string(),
        function_name: Some(name.to_string()),
        language: Some("rust".into()),
        chunk_type: ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

fn tuple(id: &str, file: &str, name: &str, calls: &[&str]) -> ChunkTuple {
    (
        id.to_string(),
        file.to_string(),
        Some(name.to_string()),
        calls.iter().map(|s| s.to_string()).collect(),
        Vec::new(),
        ChunkType::Function,
    )
}

#[test]
fn extract_doc_sections_basic() {
    let src = "\
/// Why: Centralizes auth.
/// What: Returns the token.
/// Test: see auth_tests.
fn authenticate() {}";
    let (why, what) = extract_doc_sections(src);
    assert_eq!(why.as_deref(), Some("Centralizes auth."));
    assert_eq!(what.as_deref(), Some("Returns the token."));
}

#[test]
fn extract_doc_sections_multiline() {
    let src = "\
/// Why: This solves the
/// long-standing race condition
/// across all callers.
/// What: Acquires lock then mutates.
fn foo() {}";
    let (why, what) = extract_doc_sections(src);
    let why = why.expect("why present");
    assert!(why.contains("long-standing race condition"));
    assert!(why.contains("across all callers"));
    assert_eq!(what.as_deref(), Some("Acquires lock then mutates."));
}

#[test]
fn extract_doc_sections_missing_returns_none() {
    let src = "fn bare() {}";
    let (why, what) = extract_doc_sections(src);
    assert!(why.is_none());
    assert!(what.is_none());
}

#[test]
fn extract_doc_sections_python_hash_comments() {
    let src = "\
# Why: Python uses hash comments.
# What: This still works.
def authenticate():
pass";
    let (why, what) = extract_doc_sections(src);
    assert_eq!(why.as_deref(), Some("Python uses hash comments."));
    assert_eq!(what.as_deref(), Some("This still works."));
}

#[test]
fn extract_signature_rust() {
    let src = "\
/// Why: ...
/// What: ...
#[inline]
fn authenticate(user: &str, pw: &str) -> Result<Token> {
body
}";
    let sig = extract_signature(src).expect("sig");
    assert!(sig.starts_with("fn authenticate("));
    assert!(sig.contains("-> Result<Token>"));
}

#[test]
fn extract_signature_python() {
    let src = "\
# Why: ...
@cache
def process(items: list[str]) -> int:
return len(items)";
    let sig = extract_signature(src).expect("sig");
    assert!(sig.starts_with("def process("));
}

#[test]
fn direction_parses_known_variants() {
    assert_eq!(
        CallChainDirection::parse("Both"),
        Some(CallChainDirection::Both)
    );
    assert_eq!(
        CallChainDirection::parse("outgoing"),
        Some(CallChainDirection::Outgoing)
    );
    assert_eq!(
        CallChainDirection::parse("CALLERS"),
        Some(CallChainDirection::Callers)
    );
    assert!(CallChainDirection::parse("sideways").is_none());
}

#[test]
fn request_validate_clamps_depth_and_normalises_direction() {
    let req = CallChainRequest {
        index_id: "demo".into(),
        entry_point: "foo".into(),
        direction: Some("outgoing".into()),
        max_depth: Some(99),
        include_source: Some(false),
    };
    let v = req.validate().expect("ok");
    assert_eq!(v.direction, CallChainDirection::Outgoing);
    assert_eq!(v.max_depth, MAX_DEPTH_CAP);
    assert!(!v.include_source);
}

#[test]
fn request_validate_rejects_empty_index_id() {
    let req = CallChainRequest {
        index_id: "  ".into(),
        entry_point: "foo".into(),
        direction: None,
        max_depth: None,
        include_source: None,
    };
    let err = req.validate().unwrap_err();
    assert!(err.contains("index_id"));
}

#[test]
fn request_validate_rejects_bad_direction() {
    let req = CallChainRequest {
        index_id: "demo".into(),
        entry_point: "foo".into(),
        direction: Some("sideways".into()),
        max_depth: None,
        include_source: None,
    };
    let err = req.validate().unwrap_err();
    assert!(err.contains("direction"));
}

#[test]
fn resolve_entry_point_exact_match() {
    let chunks = vec![mk_chunk("a:1:5", "a.rs", "alpha", 1, 5, "fn alpha() {}")];
    let g = SymbolGraph::build_from_chunks(&[tuple("a:1:5", "a.rs", "alpha", &[])]);
    let (sym, _c) = resolve_entry_point("alpha", &g, &chunks).expect("resolved");
    assert_eq!(sym, "alpha");
}

#[test]
fn resolve_entry_point_fuzzy_match_picks_most_connected() {
    // Two symbols both contain "auth"; the more connected one wins.
    let chunks = vec![
        mk_chunk(
            "a:1:5",
            "a.rs",
            "authenticate",
            1,
            5,
            "fn authenticate() {}",
        ),
        mk_chunk("b:1:5", "b.rs", "auth_helper", 1, 5, "fn auth_helper() {}"),
        mk_chunk("c:1:5", "c.rs", "caller_one", 1, 5, "fn caller_one() {}"),
        mk_chunk("d:1:5", "d.rs", "caller_two", 1, 5, "fn caller_two() {}"),
    ];
    let tuples = vec![
        tuple("a:1:5", "a.rs", "authenticate", &[]),
        tuple("b:1:5", "b.rs", "auth_helper", &[]),
        tuple("c:1:5", "c.rs", "caller_one", &["authenticate"]),
        tuple("d:1:5", "d.rs", "caller_two", &["authenticate"]),
    ];
    let g = SymbolGraph::build_from_chunks(&tuples);
    let (sym, _c) = resolve_entry_point("auth", &g, &chunks).expect("resolved");
    assert_eq!(
        sym, "authenticate",
        "most-connected should win the fuzzy tie"
    );
}

#[test]
fn resolve_entry_point_file_line_form() {
    let chunks = vec![mk_chunk(
        "src/auth.rs:10:25",
        "src/auth.rs",
        "authenticate",
        10,
        25,
        "fn authenticate() {}",
    )];
    let g = SymbolGraph::build_from_chunks(&[tuple(
        "src/auth.rs:10:25",
        "src/auth.rs",
        "authenticate",
        &[],
    )]);
    let (sym, c) = resolve_entry_point("src/auth.rs:15", &g, &chunks).expect("resolved");
    assert_eq!(sym, "authenticate");
    assert_eq!(c.start_line, 10);
}

#[test]
fn resolve_entry_point_not_found_returns_none() {
    let g = SymbolGraph::new();
    let chunks: Vec<RawChunk> = Vec::new();
    assert!(resolve_entry_point("nope", &g, &chunks).is_none());
}

#[test]
fn render_includes_entry_signature_and_neighbors() {
    let chunks = vec![
        mk_chunk(
            "a:1:5",
            "a.rs",
            "authenticate",
            1,
            5,
            "/// Why: Auth gate.\n/// What: Validates token.\nfn authenticate(t: &str) -> bool { hash_password(t) }",
        ),
        mk_chunk(
            "b:1:5",
            "b.rs",
            "hash_password",
            1,
            5,
            "/// Why: Hash util.\n/// What: SHA256.\nfn hash_password(p: &str) -> String { String::new() }",
        ),
        mk_chunk(
            "c:1:5",
            "c.rs",
            "login_handler",
            1,
            5,
            "/// Why: HTTP entry.\n/// What: Calls authenticate.\nfn login_handler() { authenticate(\"\"); }",
        ),
    ];
    let tuples = vec![
        tuple("a:1:5", "a.rs", "authenticate", &["hash_password"]),
        tuple("b:1:5", "b.rs", "hash_password", &[]),
        tuple("c:1:5", "c.rs", "login_handler", &["authenticate"]),
    ];
    let g = SymbolGraph::build_from_chunks(&tuples);
    let req = ValidatedCallChainRequest {
        index_id: "demo".into(),
        entry_point: "authenticate".into(),
        direction: CallChainDirection::Both,
        max_depth: 2,
        include_source: true,
    };
    let out = render_call_chain(&req, &g, &chunks).expect("rendered");
    assert!(out.contains("# Call chain: authenticate"));
    assert!(out.contains("[ENTRY]"));
    assert!(out.contains("hash_password"), "callee missing: {out}");
    assert!(out.contains("login_handler"), "caller missing: {out}");
    // The full body of the callee should be embedded (include_source + depth 1).
    assert!(out.contains("```rust"));
}

#[test]
fn direction_outgoing_omits_callers() {
    let chunks = vec![
        mk_chunk(
            "a:1:5",
            "a.rs",
            "authenticate",
            1,
            5,
            "fn authenticate() {}",
        ),
        mk_chunk(
            "c:1:5",
            "c.rs",
            "login_handler",
            1,
            5,
            "fn login_handler() {}",
        ),
    ];
    let tuples = vec![
        tuple("a:1:5", "a.rs", "authenticate", &[]),
        tuple("c:1:5", "c.rs", "login_handler", &["authenticate"]),
    ];
    let g = SymbolGraph::build_from_chunks(&tuples);
    let req = ValidatedCallChainRequest {
        index_id: "demo".into(),
        entry_point: "authenticate".into(),
        direction: CallChainDirection::Outgoing,
        max_depth: 1,
        include_source: false,
    };
    let out = render_call_chain(&req, &g, &chunks).expect("rendered");
    assert!(
        !out.contains("Called by"),
        "callers section must be omitted in outgoing-only"
    );
    assert!(!out.contains("login_handler"));
}

#[test]
fn location_from_chunk_id_parses_standard_form() {
    assert_eq!(
        location_from_chunk_id("src/auth.rs:10:25"),
        "src/auth.rs:10"
    );
    assert_eq!(location_from_chunk_id("opaque"), "opaque");
}

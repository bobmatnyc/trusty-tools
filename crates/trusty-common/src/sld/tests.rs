//! Unit tests for the `sld` grammar module (DOC-38). No I/O.

use super::*;

// ── grammar ──────────────────────────────────────────────────────────────────

#[test]
fn grammar_valid_ids() {
    for id in [
        "SPEC-SLD-02~draft",
        "SPEC-CONFORMANCE-03~draft",
        "SPEC-TUI-COORD-01~draft", // hyphenated subsystem
        "SPEC-TMMGR-01~approved",  // named accepted-state tag (DOC-36)
        "SPEC-X-99~v2",
    ] {
        assert!(is_valid_spec_id(id), "{id} should be valid");
    }
}

#[test]
fn grammar_rejects_malformed() {
    for id in [
        "SPEC-X-1~draft",  // NN needs 2+ digits
        "SPEC-x-01~draft", // subsystem must be uppercase
        "SPEC-X-01~V1",    // rev must be lowercase
        "SPEC-X-01",       // missing rev
        "spec-X-01~draft", // prefix must be SPEC-
        "SPEC-X-NN~v",     // NN must be digits
        "SPEC-X-01~",      // empty rev
    ] {
        assert!(!is_valid_spec_id(id), "{id} should be rejected");
    }
}

#[test]
fn grammar_revision_of() {
    assert_eq!(revision_of("SPEC-X-01~v2").as_deref(), Some("v2"));
    assert_eq!(
        revision_of("SPEC-X-01~approved").as_deref(),
        Some("approved")
    );
    assert_eq!(revision_of("SPEC-X-01"), None);
}

#[test]
fn grammar_base_id() {
    assert_eq!(base_id("SPEC-X-01~v2"), "SPEC-X-01");
    assert_eq!(base_id("SPEC-X-01"), "SPEC-X-01");
}

#[test]
fn grammar_reference_both_forms() {
    let link = "- [`SPEC-SLD-02~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft)";
    let bare = "SPEC-SLD-02~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft";
    for text in [link, bare] {
        let caps = reference_regex().captures(text).expect("matches");
        assert_eq!(&caps[1], "SPEC-SLD-02~draft");
        assert_eq!(&caps[2], "docs/specs/spec-linked-documentation.md");
        assert_eq!(&caps[3], "SPEC-SLD-02~draft");
    }
}

#[test]
fn grammar_rejects_traversal() {
    assert!(is_unsafe_path("../etc/x.md"));
    assert!(is_unsafe_path("docs/../secret.md"));
    assert!(!is_unsafe_path("docs/specs/x.md"));
}

#[test]
fn grammar_rejects_absolute() {
    // Unix-style absolute path: PathBuf::join would silently discard the repo
    // root and read wherever this points — this must never be treated as a
    // valid repo-root-relative reference path (§2.1).
    assert!(is_unsafe_path("/etc/passwd.md"));
    // Windows-style absolute forms, even when the linter runs on a Unix host
    // (the string form must be rejected regardless of the host platform).
    assert!(is_unsafe_path("C:\\Windows\\secret.md"));
    assert!(is_unsafe_path("C:/Windows/secret.md"));
    assert!(is_unsafe_path("\\\\server\\share\\x.md"));
    // A well-formed repo-root-relative path is unaffected.
    assert!(!is_unsafe_path("docs/specs/x.md"));
}

// ── comment ──────────────────────────────────────────────────────────────────

#[test]
fn comment_syntax_table() {
    assert!(
        syntax_for_extension("rs")
            .unwrap()
            .line_prefixes
            .contains(&"//!")
    );
    assert!(syntax_for_extension("py").unwrap().block.is_some());
    assert_eq!(syntax_for_extension("sh").unwrap().line_prefixes, &["#"]);
    // Markdown is excluded: it declares via frontmatter, not inline blocks.
    assert!(syntax_for_extension("md").is_none());
    assert!(syntax_for_extension("bin").is_none());
}

#[test]
fn comment_strip_line() {
    let rust = syntax_for_extension("rs").unwrap();
    assert_eq!(rust.strip_line_comment("//! x"), Some("x"));
    assert_eq!(rust.strip_line_comment("/// y"), Some("y"));
    // Longest prefix wins: `//!` over `//`.
    assert_eq!(rust.strip_line_comment("//!z"), Some("z"));
    assert_eq!(rust.strip_line_comment("code();"), None);
}

// ── inline ───────────────────────────────────────────────────────────────────

#[test]
fn inline_rust() {
    let src = "//! # Spec References\n//!\n//! - [`SPEC-CONFORMANCE-03~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-03~draft)\ncode();";
    let refs = parse_inline_refs(src, &syntax_for_extension("rs").unwrap());
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].id, "SPEC-CONFORMANCE-03~draft");
    assert_eq!(refs[0].path, "docs/specs/intent-conformance.md");
    assert_eq!(refs[0].anchor, "SPEC-CONFORMANCE-03~draft");
    assert_eq!(refs[0].line, 3);
}

#[test]
fn inline_bare_shell() {
    let src = "#!/bin/sh\n# Spec References\n# - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft\necho hi";
    let refs = parse_inline_refs(src, &syntax_for_extension("sh").unwrap());
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].id, "SPEC-SLD-03~draft");
    assert_eq!(refs[0].line, 3);
}

#[test]
fn inline_hash_block_closes_on_prose() {
    // HASH-only comment syntax (shell/TOML/YAML) has no nested-heading signal
    // once the lead-in `#` is stripped, so a later plain-prose comment line in
    // the SAME leading comment block must not be swept in as a declaration just
    // because it happens to contain a well-formed reference triple. Only the
    // dash-prefixed bullet (the real declaration) resolves.
    let src = "# Spec References\n# - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft\n# This mentions SPEC-OTHER-01~draft docs/specs/other.md#SPEC-OTHER-01~draft in prose\n";
    let refs = parse_inline_refs(src, &syntax_for_extension("sh").unwrap());
    assert_eq!(refs.len(), 1, "prose line must not be swept in: {refs:?}");
    assert_eq!(refs[0].id, "SPEC-SLD-03~draft");

    // Same for TOML/YAML's identical `#`-comment idiom.
    let toml_src = "# Spec References\n# - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft\n# See also SPEC-OTHER-01~draft docs/specs/other.md#SPEC-OTHER-01~draft\n\n[server]\n";
    let toml_refs = parse_inline_refs(toml_src, &syntax_for_extension("toml").unwrap());
    assert_eq!(
        toml_refs.len(),
        1,
        "prose line must not be swept in: {toml_refs:?}"
    );
}

#[test]
fn inline_skips_fenced() {
    // A fenced example inside the block must NOT be extracted; the block
    // survives the fence and the real ref after it IS extracted.
    let src = "//! # Spec References\n//! ```\n//! - [`SPEC-FAKE-99~v1`](docs/specs/fake.md#SPEC-FAKE-99~v1)\n//! ```\n//! - [`SPEC-SLD-01~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-01~draft)";
    let refs = parse_inline_refs(src, &syntax_for_extension("rs").unwrap());
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].id, "SPEC-SLD-01~draft");
}

#[test]
fn inline_ignores_non_block_ref() {
    // A reference in prose (no preceding marker) is not declared linkage.
    let src = "//! prose [`SPEC-X-01~draft`](docs/specs/x.md#SPEC-X-01~draft) inline\n//! # Spec References\n//! - [`SPEC-SLD-02~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft)";
    let refs = parse_inline_refs(src, &syntax_for_extension("rs").unwrap());
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].id, "SPEC-SLD-02~draft");
}

#[test]
fn inline_python_docstring() {
    let src = "def deploy():\n    \"\"\"Deploy.\n\n    # Spec References\n    - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft\n    \"\"\"\n    pass";
    let refs = parse_inline_refs(src, &syntax_for_extension("py").unwrap());
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].id, "SPEC-SLD-03~draft");
    assert_eq!(refs[0].line, 5);
}

#[test]
fn inline_ts_jsdoc() {
    let src = "/**\n * # Spec References\n * - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft\n */\nexport function f() {}";
    let refs = parse_inline_refs(src, &syntax_for_extension("ts").unwrap());
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].id, "SPEC-SLD-03~draft");
    assert_eq!(refs[0].line, 3);
}

// ── frontmatter ──────────────────────────────────────────────────────────────

#[test]
fn frontmatter_maps_and_shorthand() {
    let md = "---\nspec_refs:\n  - id: SPEC-SLD-03~draft\n    path: docs/specs/spec-linked-documentation.md\n    anchor: SPEC-SLD-03~draft\n  - \"SPEC-SLD-02~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft\"\n---\n# Doc\n";
    let refs = parse_frontmatter_refs(md).expect("valid");
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].id, "SPEC-SLD-03~draft");
    assert_eq!(refs[0].path, "docs/specs/spec-linked-documentation.md");
    assert_eq!(refs[0].anchor, "SPEC-SLD-03~draft");
    assert_eq!(refs[0].line, 3);
    assert_eq!(refs[1].id, "SPEC-SLD-02~draft");
    assert_eq!(refs[1].path, "docs/specs/spec-linked-documentation.md");
    assert_eq!(refs[1].line, 6);
}

#[test]
fn frontmatter_opt_in() {
    let with =
        "---\nspec_refs:\n  - \"SPEC-X-01~draft docs/specs/x.md#SPEC-X-01~draft\"\n---\n# Doc";
    let without = "# Doc\n\nNo frontmatter.";
    let other_keys = "---\ntitle: Foo\n---\n# Doc";
    assert!(has_frontmatter_spec_refs(with));
    assert!(!has_frontmatter_spec_refs(without));
    assert!(!has_frontmatter_spec_refs(other_keys));
}

#[test]
fn frontmatter_no_frontmatter_ok() {
    assert_eq!(parse_frontmatter_refs("# Just a doc\n").unwrap(), vec![]);
    // Frontmatter present but no spec_refs key → no references, not an error.
    assert_eq!(
        parse_frontmatter_refs("---\ntitle: Foo\n---\n# Doc").unwrap(),
        vec![]
    );
}

#[test]
fn frontmatter_rejects_missing_key() {
    let md = "---\nspec_refs:\n  - id: SPEC-X-01~draft\n    path: docs/specs/x.md\n---\n";
    assert!(matches!(
        parse_frontmatter_refs(md),
        Err(FrontmatterError::MissingKey { key: "anchor", .. })
    ));
}

#[test]
fn frontmatter_rejects_bad_shorthand() {
    let md = "---\nspec_refs:\n  - \"not a real reference\"\n---\n";
    assert!(matches!(
        parse_frontmatter_refs(md),
        Err(FrontmatterError::BadShorthand { .. })
    ));
}

#[test]
fn frontmatter_rejects_bad_yaml() {
    let md = "---\nspec_refs: [unclosed\n---\n";
    assert!(matches!(
        parse_frontmatter_refs(md),
        Err(FrontmatterError::Yaml(_))
    ));
}

#[test]
fn frontmatter_rejects_bad_kind() {
    let list = "---\nspec_refs:\n  - 123\n---\n";
    assert!(matches!(
        parse_frontmatter_refs(list),
        Err(FrontmatterError::BadEntryKind(0))
    ));
    let scalar = "---\nspec_refs: hello\n---\n";
    assert!(matches!(
        parse_frontmatter_refs(scalar),
        Err(FrontmatterError::NotAList)
    ));
}

// ── anchor ───────────────────────────────────────────────────────────────────

#[test]
fn anchor_scan() {
    let md = "# DOC-1\n\n## 2. Foo {#SPEC-SLD-02~draft}\ntext\n### 4.1 Bar {#SPEC-SLD-01~draft}\n";
    let a = spec_anchors(md);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].id, "SPEC-SLD-02~draft");
    assert_eq!(a[0].line, 3);
    assert_eq!(a[1].id, "SPEC-SLD-01~draft");
    assert_eq!(a[1].line, 5);
}

#[test]
fn anchor_skips_fenced() {
    let md = "## Real {#SPEC-A-01~draft}\n```\n## Fake {#SPEC-B-02~draft}\n```\n";
    let a = spec_anchors(md);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].id, "SPEC-A-01~draft");
}

#[test]
fn anchor_resolves_exact() {
    let md = "## S {#SPEC-CONFORMANCE-03~draft}\n";
    assert!(anchor_resolves(md, "SPEC-CONFORMANCE-03~draft"));
    assert!(!anchor_resolves(md, "SPEC-OTHER-01~draft"));
}

#[test]
fn anchor_resolves_across_revision() {
    // Drift is flagged, not enforced (§1.3): a ~v1 ref resolves to a ~v2 section.
    let md = "## S {#SPEC-X-01~v2}\n";
    assert!(anchor_resolves(md, "SPEC-X-01~v1"));
}

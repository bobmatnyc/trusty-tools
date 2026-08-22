//! Tests for citation → symbol resolution (#6166).
//!
//! Why: both RED findings on the engagement that drove this cite a doc-comment
//! line, so the scan-down path is the one that has to work — and the impl
//! anchoring is what keeps a bare method name from resolving into another crate.
//! What: the two scan directions, the item forms, the `Type::method` form, and
//! the unresolvable cases.
//! Test: included as `#[cfg(test)] mod tests` from `trace_symbol.rs`.

use super::*;

/// The shape `usearch_store.rs` presents at the cited line: a long doc block
/// above a `const`.
const CONST_UNDER_DOCS: &str = r"/// A guard ratio.
///
/// More prose here.
pub(super) const SHRINK_GUARD_RATIO_DIVISOR: usize = 2; // refuse below 50%
";

/// #6166: `usearch_store.rs:169` and `hnsw_store.rs:302` both land inside the
/// doc block, not on the declaration.
#[test]
fn a_doc_comment_citation_scans_down_to_the_item() {
    let got = resolve_symbol(CONST_UNDER_DOCS, 2).expect("resolves");
    assert_eq!(got.name, "SHRINK_GUARD_RATIO_DIVISOR");
    assert_eq!(got.line, 4);
}

#[test]
fn an_attribute_citation_scans_down_past_it() {
    let src = "#[derive(Debug)]\n#[non_exhaustive]\npub struct HnswStore {\n";
    let got = resolve_symbol(src, 1).expect("resolves");
    assert_eq!(got.name, "HnswStore");
    assert_eq!(got.line, 3);
}

#[test]
fn a_body_citation_scans_up_to_its_function() {
    let src = "pub fn save(&self) -> Result<()> {\n    let x = 1;\n    Ok(())\n}\n";
    let got = resolve_symbol(src, 2).expect("resolves");
    assert_eq!(got.name, "save");
    assert_eq!(got.line, 1);
}

/// A bare `save` is what #6167 mis-resolves; `UsearchStore::save` is the form
/// the call-chain endpoint disambiguates.
#[test]
fn a_method_anchors_as_type_colon_colon_method() {
    let src = "impl UsearchStore {\n    /// Doc.\n    pub async fn save(&self) -> Result<()> {\n        drop(self);\n    }\n}\n";
    assert_eq!(
        resolve_symbol(src, 2).expect("resolves").name,
        "UsearchStore::save"
    );
    assert_eq!(
        resolve_symbol(src, 4).expect("resolves").name,
        "UsearchStore::save"
    );
}

#[test]
fn an_impl_for_a_trait_anchors_on_the_self_type() {
    let src = "impl<'a> TraceSource for HttpTraceSource<'a> {\n    async fn reachable(&self) -> bool {\n        true\n    }\n}\n";
    assert_eq!(
        resolve_symbol(src, 3).expect("resolves").name,
        "HttpTraceSource::reachable"
    );
}

/// A `//` comment inside a body belongs to the code above it. Scanning down
/// from one walks past the end of the function — which refused the finding at
/// `payload_store/store.rs:413` on the live engagement before this split.
#[test]
fn a_plain_comment_inside_a_body_scans_up() {
    let src = "pub fn load_all(&self) -> Vec<u8> {\n    let p = 1;\n    // Collect into an owned Vec.\n    vec![p]\n}\n";
    assert_eq!(resolve_symbol(src, 3).expect("resolves").name, "load_all");
}

#[test]
fn a_free_function_anchors_on_its_bare_name() {
    let src = "pub fn bound(s: &str) -> String {\n    s.to_string()\n}\n";
    assert_eq!(resolve_symbol(src, 2).expect("resolves").name, "bound");
}

/// A `const` inside an `impl` keeps its bare name — only functions take the
/// `Type::` prefix.
#[test]
fn a_nested_constant_keeps_its_bare_name() {
    let src = "impl Foo {\n    /// Doc.\n    const CAP: usize = 4;\n}\n";
    assert_eq!(resolve_symbol(src, 2).expect("resolves").name, "CAP");
}

#[test]
fn declaration_covers_the_item_forms() {
    let cases = [
        ("pub fn a(x: u8) {", Some(("fn", "a"))),
        ("pub(crate) async fn b<T>() {", Some(("fn", "b"))),
        ("pub unsafe extern \"C\" fn c() {", Some(("fn", "c"))),
        ("const fn d() -> u8 {", Some(("fn", "d"))),
        ("pub struct E<T> {", Some(("struct", "E"))),
        ("pub enum F {", Some(("enum", "F"))),
        ("pub trait G: Send {", Some(("trait", "G"))),
        ("pub type H = u8;", Some(("type", "H"))),
        ("pub(super) const I: usize = 2;", Some(("const", "I"))),
        ("static J: u8 = 0;", Some(("static", "J"))),
        ("pub mod k;", Some(("mod", "k"))),
        ("    let x = 1;", None),
        ("}", None),
    ];
    for (line, want) in cases {
        let got = declaration(line);
        let got = got.as_ref().map(|(k, n)| (*k, n.as_str()));
        assert_eq!(got, want, "line: {line}");
    }
}

#[test]
fn a_non_rust_line_declares_nothing() {
    assert_eq!(declaration("export function configPane() {"), None);
    assert_eq!(declaration("fd-lock = { workspace = true }"), None);
}

/// A citation into a file with no item in range fails closed rather than
/// anchoring onto whatever declaration happens to be nearest.
#[test]
fn a_citation_with_no_declaration_is_unresolved() {
    let src = "[dependencies]\nserde = \"1\"\n";
    assert_eq!(resolve_symbol(src, 2), None);
}

#[test]
fn a_line_past_the_end_of_the_file_is_unresolved() {
    assert_eq!(resolve_symbol("pub fn a() {}\n", 99), None);
}

#[test]
fn impl_self_type_reads_the_target_not_the_trait() {
    assert_eq!(impl_self_type("impl Foo {"), Some("Foo".to_string()));
    assert_eq!(
        impl_self_type("impl Bar for Foo {"),
        Some("Foo".to_string())
    );
    assert_eq!(
        impl_self_type("impl<'a, T: Copy> Bar<T> for crate::a::Foo<'a> {"),
        Some("Foo".to_string())
    );
}

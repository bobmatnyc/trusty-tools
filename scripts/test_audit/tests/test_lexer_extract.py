"""Tests for scripts/test_audit/lexer.py and extract.py: test discovery, keys, body_sha (#11268)."""
from test_audit.extract import extract_tests
from test_audit.lexer import str_value, tokenize

SRC = r'''
//! A crate doc mentioning `#[test]` in prose, which is no test.
fn helper() -> u8 { 1 }

#[test]
fn top_level() { assert_eq!(helper(), 1); }

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_one() { assert!(true); }

    mod inner {
        #[rstest]
        fn param_case() { assert_eq!(2, 2); }

        #[test_case(1 ; "one")]
        #[test_case(2 ; "two")]
        pub(crate) fn cases(n: u8) { assert!(n > 0); }
    }

    #[cfg(unix)]
    #[test]
    fn twin() { assert_eq!(1, 1); }
    #[cfg(windows)]
    #[test]
    fn twin() { assert_eq!(2, 2); }

    macro_rules! gen { ($n:ident) => { #[test] fn $n() { assert!(true); } }; }
    gen!(made_by_macro);

    #[test]
    fn braces_in_strings() { let s = "}{ // not a comment"; assert_eq!(s.len(), 19); }
}
'''


def keys(src, crate="demo", path="crates/demo/src/lib.rs"):
    return [t.key for t in extract_tests(crate, path, src).tests]


def test_finds_every_test_attribute_shape_and_builds_keys():
    assert keys(SRC) == [
        "demo::crates/demo/src/lib.rs::top_level",
        "demo::crates/demo/src/lib.rs::tests::async_one",
        "demo::crates/demo/src/lib.rs::tests::inner::param_case",
        "demo::crates/demo/src/lib.rs::tests::inner::cases",
        "demo::crates/demo/src/lib.rs::tests::twin",
        "demo::crates/demo/src/lib.rs::tests::twin#2",
        "demo::crates/demo/src/lib.rs::tests::braces_in_strings",
    ]


def test_macro_templates_are_counted_not_listed():
    scan = extract_tests("demo", "crates/demo/src/lib.rs", SRC)
    assert scan.macro_templates == 1
    assert not any("made_by_macro" in t.key or "$n" in t.key for t in scan.tests)


def test_attr_hits_ignore_comments_and_count_code():
    # The //! mention is a comment; the 8 code attributes include the macro template.
    assert extract_tests("demo", "f.rs", SRC).attr_hits == 8


def test_integration_test_file_has_no_module_segment():
    src = "#[test]\nfn e2e() { assert_eq!(1, 1); }\n"
    assert keys(src, "demo", "crates/demo/tests/e2e.rs") == ["demo::crates/demo/tests/e2e.rs::e2e"]


def test_non_test_fns_and_cfg_test_mod_attrs_are_not_tests():
    src = "#[cfg(test)]\nfn not_a_test() {}\n#[serial_test::serial]\nfn also_not() {}\n"
    assert keys(src) == []


def sha(body_src):
    scan = extract_tests("demo", "f.rs", "#[test]\nfn t() " + body_src)
    return scan.tests[0].body_sha


def test_comment_and_whitespace_edits_keep_body_sha():
    base = sha("{ let x = 1;\n assert_eq!(x, 1); }")
    assert sha("{\n    // why: pin x\n    let x = 1; /* block */\n\n    assert_eq!(x, 1);\n}") == base
    assert sha("{ let x = 1; /// doc\n assert_eq!( x , 1 ) ; }") == base


def test_code_and_string_edits_change_body_sha():
    base = sha('{ assert_eq!(f("a b"), 1); }')
    assert sha('{ assert_eq!(f("a  b"), 1); }') != base  # literal whitespace is content
    assert sha('{ assert_eq!(f("a b"), 2); }') != base


def test_lexer_keeps_raw_strings_chars_and_lifetimes():
    toks = tokenize("let s: &'static str = r#\"a \"q\" // x\"#; let c = '}'; // gone")
    kinds = [(t.kind, t.text) for t in toks]
    assert ("life", "'static") in kinds
    assert ("str", 'r#"a "q" // x"#') in kinds
    assert ("chr", "'}'") in kinds
    assert all("gone" not in t.text for t in toks)
    assert str_value('r#"a "q""#') == 'a "q"'

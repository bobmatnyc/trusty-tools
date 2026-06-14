//! Per-thread cached tree-sitter parsers, one per supported language.
//!
//! Why: `chunk_ast` is called once per file from a rayon `par_iter` batch
//! (typically 128 files at a time). Tree-sitter `Parser` is not `Send`, so we
//! cannot share a single parser across worker threads — but a fresh parser
//! requires a C-FFI grammar load on every call, which is wasted work when the
//! same worker thread processes many files of the same language. Caching one
//! `RefCell<Parser>` per language in thread-local storage amortises the
//! grammar load to once-per-worker-per-language.
//!
//! What: declares one `thread_local!` slot per language via the
//! `ts_parser_thread_locals!` macro and exposes `parse_with_cached` which
//! borrows the right parser mutably, parses a byte slice, and returns the
//! resulting `Tree`.
//!
//! Test: existing chunker tests (`test_rust_function_chunking`,
//! `test_csharp_chunking`, etc.) exercise every language and must continue
//! to pass — the cache must not change chunk output.

use std::cell::RefCell;

use tree_sitter::{Language, Parser, Tree};

/// Identifies which cached parser to use. Distinct from the human-facing
/// `language` tag because `.ts` and `.tsx` share the `"typescript"` tag but
/// require different parsers.
///
/// Why: a single enum lets `parse_with_cached` dispatch to the correct
/// thread-local slot without runtime string matching.
/// What: one variant per supported grammar.
/// Test: covered transitively via every language-specific chunker test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ParserKind {
    Rust,
    Python,
    Javascript,
    Typescript,
    Tsx,
    Go,
    Java,
    C,
    Cpp,
    Ruby,
    Php,
    Scala,
    Csharp,
    Kotlin,
    Swift,
}

/// Per-thread cached tree-sitter parsers, one per supported language.
///
/// Why: `RefCell` is required because `Parser::parse()` takes `&mut self`;
/// this is safe because each rayon worker owns its own thread-local copy and
/// there is no cross-thread aliasing.
/// What: one `thread_local!` slot per language supported by `language_for`.
/// Each slot lazily constructs a `Parser` with the language set on first use
/// in that thread.
/// Test: existing chunker integration tests cover every slot.
macro_rules! ts_parser_thread_locals {
    ($($name:ident => $lang_expr:expr),* $(,)?) => {
        $(
            thread_local! {
                static $name: RefCell<Parser> = RefCell::new({
                    let mut p = Parser::new();
                    let lang: Language = $lang_expr.into();
                    p.set_language(&lang).expect("tree-sitter grammar load");
                    p
                });
            }
        )*
    };
}

ts_parser_thread_locals! {
    PARSER_RUST       => tree_sitter_rust::LANGUAGE,
    PARSER_PYTHON     => tree_sitter_python::LANGUAGE,
    PARSER_JAVASCRIPT => tree_sitter_javascript::LANGUAGE,
    PARSER_TYPESCRIPT => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
    PARSER_TSX        => tree_sitter_typescript::LANGUAGE_TSX,
    PARSER_GO         => tree_sitter_go::LANGUAGE,
    PARSER_JAVA       => tree_sitter_java::LANGUAGE,
    PARSER_C          => tree_sitter_c::LANGUAGE,
    PARSER_CPP        => tree_sitter_cpp::LANGUAGE,
    PARSER_RUBY       => tree_sitter_ruby::LANGUAGE,
    PARSER_PHP        => tree_sitter_php::LANGUAGE_PHP,
    PARSER_SCALA      => tree_sitter_scala::LANGUAGE,
    PARSER_CSHARP     => tree_sitter_c_sharp::LANGUAGE,
    PARSER_KOTLIN     => tree_sitter_kotlin_ng::LANGUAGE,
    PARSER_SWIFT      => tree_sitter_swift::LANGUAGE,
}

/// Borrow this thread's cached parser for `kind` and parse `src`.
///
/// Why: callers want a `Tree`, not a `Parser`; the closure form keeps the
/// `RefCell` borrow scoped tightly so it cannot leak past the parse call.
/// What: dispatches to the matching thread-local parser and returns whatever
/// `Parser::parse` produces (`None` on malformed source).
/// Test: covered transitively via every chunker integration test that exercises
/// `chunk_ast` for the listed languages.
pub(super) fn parse_with_cached(kind: ParserKind, src: &[u8]) -> Option<Tree> {
    match kind {
        ParserKind::Rust => PARSER_RUST.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Python => PARSER_PYTHON.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Javascript => PARSER_JAVASCRIPT.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Typescript => PARSER_TYPESCRIPT.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Tsx => PARSER_TSX.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Go => PARSER_GO.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Java => PARSER_JAVA.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::C => PARSER_C.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Cpp => PARSER_CPP.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Ruby => PARSER_RUBY.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Php => PARSER_PHP.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Scala => PARSER_SCALA.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Csharp => PARSER_CSHARP.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Kotlin => PARSER_KOTLIN.with(|p| p.borrow_mut().parse(src, None)),
        ParserKind::Swift => PARSER_SWIFT.with(|p| p.borrow_mut().parse(src, None)),
    }
}

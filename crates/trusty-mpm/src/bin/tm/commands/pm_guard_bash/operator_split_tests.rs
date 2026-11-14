//! Rows for the #11262 operator cut, read through the shared `tokenize`.
//!
//! Why: the cut exists so a rule never reads a word with an operator glued to
//! it; asserting on `tokenize` output is asserting on what every rule reads.
//! What: one table per operator class, plus the text that must stay whole and
//! the inputs that must refuse. Every word is neutral.
//! Test: itself.

use super::super::{TokenizeError, rough_words, tokenize, unplaced_word};
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_command;

/// Assert each `(segment, words)` row lexes to exactly `words`.
fn assert_rows(rows: &[(&str, &[&str])]) {
    for (segment, want) in rows {
        let got = tokenize(segment);
        let want: Vec<String> = want.iter().map(|w| (*w).to_string()).collect();
        assert_eq!(got, Ok(want), "segment {segment:?}");
    }
}

/// Every redirection spelling ends the word it is glued to; an fd number or a
/// `{name}` before it is part of the operator, and the target stays glued to
/// the operator as `shlex` already gives `cmd >out`.
#[test]
fn operator_split_cuts_each_redirect_class() {
    assert_rows(&[
        ("foo>bar", &["foo", ">bar"]),
        ("foo>>bar", &["foo", ">>bar"]),
        ("foo>|bar", &["foo", ">|bar"]),
        ("foo>>|bar", &["foo", ">>|bar"]),
        ("foo>&|bar", &["foo", ">&|bar"]),
        ("foo&>|bar", &["foo", "&>|bar"]),
        // #11262 review: read as a clobber, as `is_clobber_bar` reads its `|`.
        ("foo&>>|bar", &["foo", "&>>|bar"]),
        ("foo>&2", &["foo", ">&2"]),
        ("foo&>bar", &["foo", "&>bar"]),
        ("foo&>>bar", &["foo", "&>>bar"]),
        ("foo<bar", &["foo", "<bar"]),
        ("foo<>bar", &["foo", "<>bar"]),
        ("foo<&3", &["foo", "<&3"]),
        ("foo<<<bar", &["foo", "<<<bar"]),
        ("foo<<-END", &["foo", "<<-END"]),
        ("foo>bar<baz", &["foo", ">bar", "<baz"]),
        ("x=1>bar", &["x=1", ">bar"]),
        ("'foo'>bar", &["foo", ">bar"]),
        // An fd number only counts as the whole word before the operator.
        ("foo 2>bar", &["foo", "2>bar"]),
        ("foo 2>&1", &["foo", "2>&1"]),
        ("foo 12>bar", &["foo", "12>bar"]),
        ("foo2>bar", &["foo2", ">bar"]),
        ("foo2>&1", &["foo2", ">&1"]),
        ("foo '2'>bar", &["foo", "2", ">bar"]),
        // `&>` takes no fd number, so the digits are an argument.
        ("foo 2&>bar", &["foo", "2", "&>bar"]),
        ("foo {fd}>bar", &["foo", "{fd}>bar"]),
        ("foo a{fd}>bar", &["foo", "a{fd}", ">bar"]),
        // A redirect target followed by another operator ends there too.
        ("foo >bar>baz", &["foo", ">bar", ">baz"]),
    ]);
}

/// A control operator ends the word on both sides. A subshell `(` and `)`
/// stay glued as `shlex` leaves them, since the rules read `(cmd` today.
#[test]
fn operator_split_cuts_control_operators() {
    assert_rows(&[
        ("foo;bar", &["foo", ";", "bar"]),
        ("foo&", &["foo", "&"]),
        ("foo&&bar", &["foo", "&&", "bar"]),
        ("foo||bar", &["foo", "||", "bar"]),
        ("foo|bar", &["foo", "|", "bar"]),
        ("foo|&bar", &["foo", "|&", "bar"]),
        ("foo;;", &["foo", ";;"]),
        // #11262 review: bash 5 reads each as one case terminator.
        ("foo;&bar", &["foo", ";&", "bar"]),
        ("foo;;&bar", &["foo", ";;&", "bar"]),
        ("foo;#bar", &["foo", ";"]),
        ("(foo bar)", &["(foo", "bar)"]),
        ("(foo bar)>baz", &["(foo", "bar)", ">baz"]),
        ("(foo bar<baz)", &["(foo", "bar", "<baz)"]),
        ("a=(b c)", &["a=(b", "c)"]),
    ]);
}

/// Quoted, escaped, here-document and substitution text is never cut at the
/// outer level.
#[test]
fn operator_split_leaves_quoted_and_nested_text_whole() {
    assert_rows(&[
        ("'foo>bar'", &["foo>bar"]),
        ("\"foo>bar;baz\"", &["foo>bar;baz"]),
        ("foo\\>bar", &["foo>bar"]),
        ("foo\\;bar\\&", &["foo;bar&"]),
        ("$'a>b'", &["$a>b"]),
        ("foo$(a>b)", &["foo$(a>b)"]),
        ("\"x$(a>b)y\"", &["x$(a>b)y"]),
        ("foo`a>b`", &["foo`a>b`"]),
        ("foo${a:->b}", &["foo${a:->b}"]),
        ("foo$((1>0))", &["foo$((1>0))"]),
        ("((1>0))", &["((1>0))"]),
        ("foo<(a>b)", &["foo<(a>b)"]),
        ("foo>(a)", &["foo>(a)"]),
        ("foo $(a>b", &["foo", "$(a>b"]),
        ("foo #a>b", &["foo"]),
        (
            "foo<<END\nfoo>bar\nEND",
            &["foo", "<<END", "foo>bar", "END"],
        ),
    ]);
}

/// What the cut cannot place refuses through the tokenizer's error arm, and an
/// unbalanced quote still reports as one.
#[test]
fn operator_split_refuses_what_it_cannot_place() {
    // bash reads `$'…'` escapes; `shlex` does not, so the two disagree on
    // whether the `>` is quoted.
    assert_eq!(
        tokenize("$'a\\'b'>c'"),
        Err(TokenizeError::AmbiguousOperators)
    );
    // A `case` pattern's `)` inside a substitution is not tracked.
    assert_eq!(
        tokenize("foo $(case a in b) c>d;; esac)"),
        Err(TokenizeError::AmbiguousOperators)
    );
    assert!(
        TokenizeError::AmbiguousOperators
            .reason()
            .contains("cannot parse")
    );
    assert_eq!(tokenize("foo>'bar"), Err(TokenizeError::UnbalancedQuoting));
}

/// Commands the guard refused before #11262 still refuse with the cut in
/// place.
#[test]
fn operator_split_keeps_refusing_glued_secret_reads() {
    for command in [
        "cat .env>out.txt",
        "cat .env 2>/dev/null",
        "cat .env<in.txt",
        "cat x.txt .env&",
        "(cat .env)",
    ] {
        assert!(
            evaluate_secret_file_read_command(command).is_some(),
            "{command:?} must still refuse"
        );
    }
}

/// The words a rule reads in a segment it cannot place: cut at quotes and
/// operators, and read whole across a quote join; only the unplaceable error
/// lets a rule act on them.
#[test]
fn rough_words_read_quote_joins_and_glued_operators() {
    let words = rough_words("foo;bar>baz 'ab''c' \\qux");
    for want in ["foo", "bar", "baz", "ab", "c", "abc", "qux"] {
        assert!(words.iter().any(|w| w == want), "{want} in {words:?}");
    }
    let names_bar = |w: &str| w == "bar";
    assert!(unplaced_word(
        TokenizeError::AmbiguousOperators,
        "foo;bar",
        names_bar
    ));
    assert!(!unplaced_word(
        TokenizeError::UnbalancedQuoting,
        "foo;bar",
        names_bar
    ));
    assert!(!unplaced_word(
        TokenizeError::AmbiguousOperators,
        "foo;baz",
        names_bar
    ));
}

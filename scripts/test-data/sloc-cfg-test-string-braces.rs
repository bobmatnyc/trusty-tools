//! Fixture: braces inside string, byte-string, and char literals inside a
//! `#[cfg(test)] mod`, plus lifetimes that must NOT be read as char literals.
//! The module is excluded; only the 6 production lines below count.
//!
//! The byte-string line reproduces the shape that broke this in
//! `crates/trusty-search/src/core/corpus/contrib.rs` and made
//! scripts/check_teardown_guard.sh report ten test fixtures as unguarded
//! production writers.
//!
//! The `longest` line is the guard on the fix itself: `'a` is a lifetime, not
//! a char literal. A scanner that treated every `'` as opening one would blank
//! the rest of that line, lose its trailing `}`, and stop excluding this
//! module — so this fixture fails if anyone "improves" the char-literal rule
//! into a general one.

pub struct Holder<'a> {
    pub name: &'a str,
}

pub fn parse() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAD: &[u8] = b"{ not a contrib graph";
    const URL: &str = "https://example.test/a//b{c";
    const OPEN: char = '{';
    const CLOSE: char = '}';
    const ESCAPED: char = '\'';

    fn longest<'a>(x: &'a str, _y: &'a str) -> &'a str { x }

    #[test]
    fn t() {
        assert_eq!(parse(), 1);
        assert_eq!(longest("a", "b"), "a");
        assert_eq!(OPEN, '{');
        assert_eq!(CLOSE, '}');
        assert!(!BAD.is_empty() && !URL.is_empty() && ESCAPED == '\'');
    }
}

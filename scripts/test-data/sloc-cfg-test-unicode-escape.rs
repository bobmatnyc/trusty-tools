//! Fixture: a `'\u{7B}'` char literal inside a `#[cfg(test)] mod`. The module
//! is excluded; only the 3 production lines below count.
//!
//! `\u{XXXX}` is the one way a brace GLYPH reaches the brace counter from
//! inside a char literal, because sloc_awk.sh's scanner consumes only the
//! exact `'X'` and `'\X'` forms — a lifetime like `'a` must not open a
//! literal, so nothing longer is consumed. The escape is safe there only
//! because it is self-balanced: exactly one `{` and one `}`, so the line's
//! net delta is unchanged.
//!
//! That reasoning used to exist only as prose. This fixture is the check. The
//! escape shares a line with a real code brace pair, so it fails the moment
//! the `'\X'` rule stops verifying its closing quote: `i += 4` would then eat
//! `'\u{` and leave the `}` behind, the balance would go one short, and the
//! module would stop being excluded (3 SLOC -> 9).

pub fn brace_char() -> char {
    '{'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_escape_is_self_balanced() { assert_eq!(brace_char(), '\u{7B}'); }
}

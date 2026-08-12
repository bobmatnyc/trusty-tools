//! Fixture: a raw string holding an unmatched `{`. The brace counter blanks
//! string contents before balancing (`brace_text` in scripts/lib/sloc_awk.sh),
//! so the module still balances and IS excluded — only the 3 production lines
//! count.
//!
//! This fixture used to pin the opposite expectation (11 SLOC, module counted),
//! on the premise that a line-based counter cannot know a `{` sits inside a
//! literal. It now can, so the premise is retired rather than the protection:
//! the fail-closed half of the matcher is pinned by -unterminated.rs and
//! -other-predicate.rs, and the case where trusting the balance would be wrong
//! is pinned by -string-braces.rs's lifetime line.

pub fn parse() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    const BROKEN: &str = r#"{ "unclosed": true "#;

    #[test]
    fn t() {
        assert_eq!(parse(), 1);
    }
}

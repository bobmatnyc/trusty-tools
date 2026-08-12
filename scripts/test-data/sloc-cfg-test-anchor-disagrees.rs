//! Fixture: DIRECTION assertion #1 — brace balance agrees, indent anchor does
//! not, so NOTHING is excluded and all 7 lines count.
//!
//! Why this rather than a fixture built from some construct the scanner
//! mis-lexes: in valid Rust an unmatched brace can only come from a literal or
//! a comment, and the scanner blanks every literal form there is (normal,
//! byte, raw-with-hashes, `'X'`, `'\X'`). Probing `'\u{1F600}'`, `'\u{7b}'`,
//! `b'{'`, `r##"a "# b {"##`, `"\"{"`, a lifetime, and `println!("{{")` found
//! no shape that unbalances it. A fixture asserting "this specific input is
//! lexed right" would only ever prove what it names.
//!
//! So this pins the INVARIANT that protects against the hole nobody has found
//! yet. Teaching the matcher about literals is structurally a change that
//! makes it exclude MORE, which is where a silent false negative would enter.
//! The thing standing between a future lexing mistake and a wrongly-excluded
//! region is that exclusion needs TWO independent signals to agree: the brace
//! balance must return to zero AND that line must be exactly the anchor indent
//! plus `}`. Here the balance returns to zero on the `} }` line, which is not
//! the anchor, so the region is counted.
//!
//! Keep this at 7. If it drops, a single signal has become sufficient, and the
//! fail-closed bias that makes both consumers of this detector safe is gone —
//! for every shape, not just this one.

pub fn parse() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() { assert_eq!(parse(), 1); } }

//! Fixture: DIRECTION assertion, not a count correction.
//!
//! Teaching the matcher about literals is structurally a change that makes it
//! exclude MORE, so for the first time a lexing mistake here could mean "skip"
//! rather than "count" — a silent under-count with no other gate behind it.
//! This fixture pins the direction: a region opener that exists only INSIDE a
//! raw string is a decoy, and treating it as real would exclude the production
//! code below it.
//!
//! A lexer that loses track of the raw string sees `#[cfg(test)] mod tests {`
//! at column 0, finds the `}` at column 0 four lines later, and excludes both
//! — silently dropping real production lines from the count. Correct
//! behaviour is to exclude NOTHING here, so all 11 lines count.
//!
//! This is not hypothetical and it is the one place this change makes a count
//! go UP: the PREVIOUS matcher scored this file skip=4, sloc=7. It fell for
//! the decoy and excluded four lines of a string literal as though they were a
//! test module. So the primitive did not have a single failure direction
//! before this change — it could already drop real lines silently. It scores
//! skip=0, sloc=11 now.
//!
//! Keep the expectation at 11. If a future edit makes this fixture DROP, the
//! matcher has started excluding on evidence it cannot actually read, and the
//! one-directional failure property that makes both consumers safe is gone.

pub fn emit_template() -> &'static str {
    r#"
#[cfg(test)]
mod tests {
    fn decoy() {}
}
"#
}

pub fn real() -> u8 {
    2
}

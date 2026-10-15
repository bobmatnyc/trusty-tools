//! Reading `vercel env ls <target>` output, failing closed (#9069 PR 2).
//!
//! Why: V6, D8 — a key counts as pushed only when the listing names it on
//! every target, so output this parser does not recognise must never read
//! as "listed" or as a clean "empty".
//! What: [`names`], the variable names a listing's table shows.
//!
//! UNCONFIRMED: the table shape (a `name  value  environments …` header,
//! then one row per variable whose first column is its name) and the
//! "No Environment Variables found" notice follow the CLI's documented
//! output; the #9069 live check pins them, and which stream each goes to.
//! Test: `vercel_listing_parser_table`.

use std::collections::BTreeSet;

/// The first header columns of the listing's table, lowercased.
const HEADER: [&str; 3] = ["name", "value", "environments"];

/// The notice a target with no variables prints instead of a table.
const NONE_FOUND: &str = "no environment variables found";

/// The variable names `stdout` lists, or `None` when it is not a listing.
///
/// What: lines before the table header are skipped, except that a line
/// holding [`NONE_FOUND`] before any header is an empty listing. After the
/// header, every non-blank line is a row whose first column must be a
/// variable name (`[A-Za-z_][A-Za-z0-9_]*`); any other row, or no header
/// and no notice, is `None`.
/// Test: `vercel_listing_parser_table`.
pub(super) fn names(stdout: &str) -> Option<BTreeSet<String>> {
    // #9069: red skeleton.
    let _ = (stdout, HEADER, NONE_FOUND);
    None
}

/// Whether `name` is a Vercel environment variable name.
///
/// What: letters, digits and `_`, not starting with a digit.
pub(super) fn is_variable_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

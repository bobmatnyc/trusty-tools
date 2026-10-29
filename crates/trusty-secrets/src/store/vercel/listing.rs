//! Reading `vercel env ls <target>` output, failing closed (#9069 PR 2).
//!
//! Why: V6, D8 — a key counts as pushed only when the listing names it on
//! every target, so output this parser does not recognise must never read
//! as "listed" or as a clean "empty".
//! What: [`names`], the variable names a listing's table shows.
//!
//! The table shape is the live CLI's: a `name  value  type  environments
//! created` header, then one row per variable whose first column is its
//! name. An empty target prints its "No Environment Variables found" notice
//! on stderr only, so empty stdout is not a listing (#9927).
//! Test: `vercel_listing_parser_table`, `vercel_listing_parser_real_cli_table`.

use std::collections::BTreeSet;

/// The header columns every listing carries, lowercased: it starts with
/// `name value` and names an `environments` column after them (a `type`
/// column may sit between, #9927).
const HEADER_START: [&str; 2] = ["name", "value"];
const HEADER_ENVIRONMENTS: &str = "environments";

/// The notice a target with no variables prints instead of a table.
const NONE_FOUND: &str = "no environment variables found";

/// The variable names `stdout` lists, or `None` when it is not a listing.
///
/// What: lines before the table header are skipped, except that a line
/// holding [`NONE_FOUND`] before any header is an empty listing. After the
/// header, every non-blank line is a row whose first column must be a
/// variable name (`[A-Za-z_][A-Za-z0-9_]*`); any other row, or no header
/// and no notice, is `None`.
/// Test: `vercel_listing_parser_table`, `vercel_listing_parser_real_cli_table`.
pub(super) fn names(stdout: &str) -> Option<BTreeSet<String>> {
    let mut rows = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    loop {
        let line = rows.next()?.to_ascii_lowercase();
        let columns: Vec<&str> = line.split_whitespace().collect();
        if columns.starts_with(&HEADER_START) && columns.contains(&HEADER_ENVIRONMENTS) {
            break;
        }
        if line.contains(NONE_FOUND) {
            return Some(BTreeSet::new());
        }
    }
    rows.map(|row| {
        let first = row.split_whitespace().next()?;
        is_variable_name(first).then(|| first.to_string())
    })
    .collect()
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

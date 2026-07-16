//! Spec-catalog parsing (DOC-38 §4.5).
//!
//! Why: DOC-38 requires every spec to have a row in its repository's catalog
//! (in trusty-tools, the `docs/specs/README.md` table). The linter's catalog-row
//! check needs the set of cataloged `DOC-N` numbers and each spec file's
//! self-labeled number to compare them.
//! What: [`parse_catalog`] extracts the `DOC-N` numbers from the README's table
//! rows; [`doc_number_of`] reads a spec file's self-labeled `DOC-N`.
//! Test: `super::tests::catalog_parses_rows`, `catalog_doc_number_of`.

use std::collections::HashSet;

/// Extract the set of cataloged `DOC-N` numbers from the spec-catalog README.
///
/// Why: the catalog-row check must know which document numbers already have a
/// row so it can flag a self-labeled spec that is missing one (§4.5). Only table
/// rows count — prose catalog *notes* (which begin with `>`) deliberately
/// mention uncataloged numbers (the documented DOC-28/34 exceptions) and must
/// NOT be read as rows.
/// What: returns every `DOC-N` number found at the start of a Markdown table row
/// (a line whose trimmed form begins with `|`).
/// Test: `super::tests::catalog_parses_rows`.
#[must_use]
pub fn parse_catalog(readme: &str) -> HashSet<u32> {
    readme
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .filter_map(first_doc_number)
        .collect()
}

/// Read a spec file's self-labeled `DOC-N` number, if it has one.
///
/// Why: the catalog-row check compares a spec's own number against the catalog;
/// some specs (e.g. `SPEC-INSTALLER-01.md`) carry no `DOC-N` and are simply
/// skipped by that check. The self-label is specifically the document's own
/// `# DOC-N — Title` H1 heading (DOC-38 §4.2's title-heading convention) — NOT
/// just any `DOC-N` mention earlier in the file. A spec's `spec_refs:`
/// frontmatter, a superseded/redirect banner, or body prose can legitimately
/// cross-reference a *different* DOC-N before the file's own title heading
/// appears; scanning for the first `DOC-` occurrence anywhere would misattribute
/// the file to that other spec's number instead of its own.
/// What: finds the first line whose trimmed form starts with `# ` (a Markdown
/// H1 — frontmatter lines like `---`/`spec_refs:` do not match, so a leading
/// frontmatter block is skipped automatically) and parses the `DOC-N` number
/// from that line only. Returns `None` when there is no H1 heading, or the H1
/// carries no `DOC-N` label.
/// Test: `super::tests::catalog_doc_number_of`,
/// `super::tests::catalog_doc_number_ignores_earlier_cross_reference`.
#[must_use]
pub fn doc_number_of(spec: &str) -> Option<u32> {
    spec.lines()
        .find(|l| l.trim_start().starts_with("# "))
        .and_then(first_doc_number)
}

/// Parse the first `DOC-<n>` number on a line.
///
/// Why: both the catalog scan and the self-label read reduce to "find `DOC-`
/// then take the following digits"; one helper keeps the parse identical.
/// What: locates `DOC-` and parses the immediately following ASCII digits as a
/// `u32`; returns `None` when absent or non-numeric.
/// Test: covered by `super::tests::catalog_parses_rows`.
fn first_doc_number(line: &str) -> Option<u32> {
    let idx = line.find("DOC-")?;
    let digits: String = line[idx + 4..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

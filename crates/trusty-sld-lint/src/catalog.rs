//! Spec-catalog parsing (DOC-38 §4.5).
//!
//! Why: DOC-38 requires every spec to have a row in its repository's catalog
//! (in trusty-tools, the `docs/specs/README.md` table). The linter's catalog-row
//! check needs the set of cataloged `DOC-N` numbers and each spec file's
//! self-labeled number to compare them.
//! What: [`parse_catalog`] extracts the `DOC-N` numbers from the README's table
//! rows; [`doc_number_of`] reads a spec file's self-labeled `DOC-N` — either
//! the `# DOC-N — Title` H1 form or, when the H1 carries no number, a
//! `**DOC-N** | Status: …` bold lead-line fallback (DOC-32/DOC-33's
//! convention).
//! Test: `super::tests::catalog_parses_rows`, `catalog_doc_number_of`,
//! `catalog_doc_number_ignores_earlier_cross_reference`,
//! `catalog_doc_number_bold_self_label_convention`,
//! `catalog_doc_number_bold_label_ignores_bare_cross_reference`,
//! `catalog_doc_number_none_when_truly_unlabeled`,
//! `catalog_doc_number_detects_doc28_self_label_despite_collision_note`.

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

/// How many leading lines the `**DOC-N**` self-label fallback scans.
///
/// Why: the fallback (see [`doc_number_of`]) must stay close to the top of the
/// file — a self-label lead line always sits immediately under the title
/// (DOC-32/DOC-33 carry it on line 3) — so a window, not the whole document,
/// keeps a later body cross-reference from ever being mistaken for the file's
/// own label.
const SELF_LABEL_SCAN_LINES: usize = 15;

/// Read a spec file's self-labeled `DOC-N` number, if it has one.
///
/// Why: the catalog-row check compares a spec's own number against the catalog;
/// some specs (e.g. `SPEC-INSTALLER-01.md`, `trusty-memory-chat-session-manager.md`)
/// carry no `DOC-N` anywhere near their top and are simply skipped by that
/// check. Most specs self-label in the `# DOC-N — Title` H1 heading (DOC-38
/// §4.2); a few (`DOC-32`, `DOc-33` — `tool-output-interception-seam.md`,
/// `tm-meta-harness-logging.md`) instead give the H1 a plain title and
/// self-label on a separate `**DOC-N** | Status: …` lead line just below it.
/// Trusting only the H1 (the prior, narrower fix) silently stopped running the
/// catalog-row check on those two files — a verified regression, since it used
/// to run (correctly) before that fix. Both forms are still scanned narrowly
/// (H1 first; failing that, a strict `**DOC-N** | Status:`-shaped line within
/// the first few lines) so a `spec_refs:` frontmatter cross-reference, a
/// superseded/redirect banner, or body prose mentioning a *different* DOC-N
/// still can never win over the file's own label (a bare `**DOC-15**` mention
/// with no trailing `| Status:` marker does not match the fallback pattern).
/// What: first tries the first line whose trimmed form starts with `# ` (a
/// Markdown H1 — frontmatter lines like `---`/`spec_refs:` do not match, so a
/// leading frontmatter block is skipped automatically) and parses a `DOC-N`
/// number from it. If that H1 carries no `DOC-N`, falls back to the first of
/// the document's leading [`SELF_LABEL_SCAN_LINES`] lines that matches the
/// `**DOC-N** | …` self-label shape. Returns `None` only when neither form is
/// present.
/// Test: `super::tests::catalog_doc_number_of`,
/// `super::tests::catalog_doc_number_ignores_earlier_cross_reference`,
/// `super::tests::catalog_doc_number_bold_self_label_convention`,
/// `super::tests::catalog_doc_number_bold_label_ignores_bare_cross_reference`,
/// `super::tests::catalog_doc_number_none_when_truly_unlabeled`.
#[must_use]
pub fn doc_number_of(spec: &str) -> Option<u32> {
    if let Some(n) = spec
        .lines()
        .find(|l| l.trim_start().starts_with("# "))
        .and_then(first_doc_number)
    {
        return Some(n);
    }
    spec.lines()
        .take(SELF_LABEL_SCAN_LINES)
        .find_map(bold_self_label)
}

/// Parse the first `DOC-<n>` number on a line.
///
/// Why: both the catalog scan and the H1 self-label read reduce to "find
/// `DOC-` then take the following digits"; one helper keeps the parse identical.
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

/// Parse a `**DOC-N** | …` bold self-label lead line.
///
/// Why: DOC-32/DOC-33's convention puts the self-label on its own line,
/// immediately bold-wrapped and followed by a `|`-delimited status field
/// (`**DOC-33** | Status: \`Draft\` | Date: 2026-07-03`). Requiring the
/// trailing `|` — not just a bare `**DOC-N**` — is what stops a bold
/// cross-reference mention (e.g. `**DOC-15** is related background`, which has
/// no `|` immediately after) from being mistaken for the file's own label.
/// What: returns the number when the trimmed line starts with `**DOC-`,
/// immediately followed by ASCII digits and a closing `**`, and (after
/// trimming) the remainder starts with `|`. Returns `None` otherwise.
/// Test: `super::tests::catalog_doc_number_bold_self_label_convention`,
/// `super::tests::catalog_doc_number_bold_label_ignores_bare_cross_reference`.
fn bold_self_label(line: &str) -> Option<u32> {
    let rest = line.trim_start().strip_prefix("**DOC-")?;
    let digit_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let digits = &rest[..digit_end];
    if digits.is_empty() {
        return None;
    }
    let after = rest[digit_end..].strip_prefix("**")?;
    if !after.trim_start().starts_with('|') {
        return None;
    }
    digits.parse().ok()
}

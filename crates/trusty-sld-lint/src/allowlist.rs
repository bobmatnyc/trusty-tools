//! Grandfather allowlist (ratchet), mirroring `.line-cap-allowlist.tsv`.
//!
//! Why: DOC-38 documents pre-existing exceptions — the DOC-28 self-label
//! collision and the DOC-34 catalog gap (§4.1, §9) — that must NOT fail the
//! lint. A ratcheted allowlist suppresses exactly those `(path, check)` pairs, so
//! `--strict` is usable today while the retrofit follow-ups (§10 F3/F5/F6) land,
//! and the list can only shrink as they do.
//! What: [`AllowEntry`] (one `<path><TAB><check>` row), [`parse`] (read the TSV,
//! skipping blanks and `#` comments), and [`suppresses`] (does any entry match a
//! diagnostic?).
//! Test: `super::tests::allowlist_suppresses`.

use crate::report::Diagnostic;

/// One grandfathered exception: a `(path, check)` pair to suppress.
///
/// Why: an exception is scoped to a specific file AND a specific check so a
/// blanket suppression can never hide an unrelated new violation.
/// What: the repo-relative `path` and the stable `check` id to suppress for it.
/// Test: `super::tests::allowlist_suppresses`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowEntry {
    /// Repo-relative path the exception applies to.
    pub path: String,
    /// The stable check id suppressed for `path`.
    pub check: String,
}

/// Parse the allowlist TSV (`<path><TAB><check>` per row).
///
/// Why: the allowlist is a plain, reviewable TSV like the line-cap ratchet;
/// parsing it needs no dependency and tolerates comments/blanks.
/// What: returns one [`AllowEntry`] per non-blank, non-`#` line with exactly a
/// path and a check separated by a tab; malformed lines are skipped.
/// Test: `super::tests::allowlist_suppresses`.
#[must_use]
pub fn parse(tsv: &str) -> Vec<AllowEntry> {
    tsv.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let (path, check) = l.split_once('\t')?;
            let (path, check) = (path.trim(), check.trim());
            (!path.is_empty() && !check.is_empty()).then(|| AllowEntry {
                path: path.to_string(),
                check: check.to_string(),
            })
        })
        .collect()
}

/// True when some allowlist entry suppresses this diagnostic.
///
/// Why: a grandfathered `(path, check)` must be filtered out before the lint
/// decides pass/fail, so documented exceptions do not fail CI.
/// What: returns `true` when any entry's `path` and `check` both equal the
/// diagnostic's.
/// Test: `super::tests::allowlist_suppresses`.
#[must_use]
pub fn suppresses(allow: &[AllowEntry], diag: &Diagnostic) -> bool {
    allow
        .iter()
        .any(|e| e.check == diag.check && e.path == diag.path)
}

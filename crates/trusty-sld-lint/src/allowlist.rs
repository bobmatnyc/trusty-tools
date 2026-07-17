//! Grandfather allowlist (ratchet), mirroring `.line-cap-allowlist.tsv`.
//!
//! Why: DOC-38 documents pre-existing exceptions — the DOC-28 self-label
//! collision and the DOC-34 catalog gap (§4.1, §9) — that must NOT fail the
//! lint. A ratcheted allowlist suppresses exactly those `(path, check)` pairs, so
//! `--strict` is usable today while the retrofit follow-ups (§10 F3/F5/F6) land.
//! "Ratchet" is mechanically enforced on the READ side by [`stale_entries`]: an
//! entry that stops matching any real violation (the file was actually fixed)
//! fails the lint until the entry is removed, so the list can only shrink, never
//! silently accumulate. There is currently NO write-side guard against adding a
//! new entry (no `--update`/`--seed` generator like `check_line_cap.sh`'s); that
//! remains a manual, reviewed edit — tracked as a follow-up
//! (see the PR/issue history for #2854).
//! What: [`AllowEntry`] (one `<path><TAB><check>` row), [`parse`] (read the TSV,
//! skipping blanks and `#` comments), [`suppresses`] (does any entry match a
//! diagnostic?), and [`stale_entries`] (which entries matched NOTHING — the
//! ratchet-down forcing function).
//! Test: `super::tests::allowlist_suppresses`, `allowlist_stale_entry_flagged`.

use crate::report::Diagnostic;
use crate::ALLOWLIST_FILE;

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

/// Allowlist entries that suppress nothing — the ratchet-down forcing function.
///
/// Why: a grandfathered exception documents a violation that exists TODAY; once
/// the underlying file is fixed, the entry stops matching any real diagnostic
/// and MUST be removed, or the allowlist silently accumulates dead rows and the
/// "ratchet" claim in this module's header becomes fiction. Emitting a hard
/// failure for a stale entry — mirroring `check_line_cap.sh`'s "now under cap,
/// remove it" rule — is what makes the ratchet mechanical rather than aspirational.
/// What: returns one `Diagnostic::error("allowlist-stale", …)` per entry in
/// `allow` whose `(path, check)` matches none of `diagnostics` (the FULL,
/// pre-suppression set — the caller must pass diagnostics from a run where the
/// entry's check actually applies to its file; see [`crate::run`]'s `--strict`
/// gating note).
/// Test: `super::tests::allowlist_stale_entry_flagged`,
/// `super::tests::allowlist_live_entry_not_flagged`.
#[must_use]
pub fn stale_entries(allow: &[AllowEntry], diagnostics: &[Diagnostic]) -> Vec<Diagnostic> {
    allow
        .iter()
        .filter(|e| {
            !diagnostics
                .iter()
                .any(|d| d.check == e.check && d.path == e.path)
        })
        .map(|e| {
            Diagnostic::error(
                ALLOWLIST_FILE,
                0,
                "allowlist-stale",
                format!(
                    "grandfathered entry `{}\t{}` no longer matches any violation — remove it from {ALLOWLIST_FILE} (ratchet can only shrink)",
                    e.path, e.check
                ),
            )
        })
        .collect()
}

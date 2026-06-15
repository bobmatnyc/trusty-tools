//! Branch-name derivation for `tm ticket`.
//!
//! Why: the issue → branch step must turn a free-form issue title plus its
//! labels into a deterministic, git-safe branch name (`<type>/<issue#>-<slug>`)
//! without invoking git or the network, so it can be unit-tested in isolation
//! and reproduced identically on every run.
//! What: pure functions — `slugify` (title → kebab slug), `issue_type` (labels →
//! conventional-commit type), and `derive_branch_name` (the composed result).
//! Test: `slugify_*`, `issue_type_*`, and `derive_branch_name_*` in this file's
//! `#[cfg(test)]` block.

/// Maximum number of slug characters appended after the issue number.
///
/// Why: git branch names should stay short and human-readable; an arbitrarily
/// long issue title would otherwise produce an unwieldy ref. 50 chars is enough
/// to stay descriptive while bounding the worst case.
/// What: the slug is truncated at a word boundary at or below this length.
/// Test: `slugify_truncates_long_title`.
const MAX_SLUG_LEN: usize = 50;

/// Convert a free-form issue title into a git-safe kebab-case slug.
///
/// Why: branch names may only contain a restricted character set; titles contain
/// spaces, punctuation, and mixed case that must be normalised to a predictable
/// form so the same title always yields the same branch. Non-ASCII titles
/// (Japanese, accented Latin, Arabic) must survive slugification rather than
/// collapsing to the `issue` fallback and colliding with every other non-ASCII
/// title.
/// What: lowercases, keeps Unicode alphanumerics (`char::is_alphanumeric`),
/// replaces any run of other characters with a single `-`, trims leading/trailing
/// `-`, then truncates to [`MAX_SLUG_LEN`] at the last `-` boundary so words are
/// never cut mid-token. Returns `issue` only when the title has no usable
/// alphanumeric characters at all.
/// Test: `slugify_basic`, `slugify_collapses_punctuation`,
/// `slugify_truncates_long_title`, `slugify_empty_falls_back`,
/// `slugify_preserves_accented_latin`, `slugify_preserves_cjk`.
pub(crate) fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        return "issue".to_string();
    }
    if trimmed.len() <= MAX_SLUG_LEN {
        return trimmed.to_string();
    }
    // Truncate at the last dash at or before the cap so we never split a word.
    // Find a UTF-8 char boundary at or below the cap first, since non-ASCII
    // slugs may contain multi-byte codepoints and slicing mid-codepoint panics.
    let mut end = MAX_SLUG_LEN;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &trimmed[..end];
    match cut.rfind('-') {
        Some(idx) if idx > 0 => cut[..idx].to_string(),
        _ => cut.trim_end_matches('-').to_string(),
    }
}

/// Pick a conventional-commit branch type from a set of issue labels.
///
/// Why: the repo convention is `<type>/<issue#>-<slug>` where `<type>` is a
/// conventional-commit kind; deriving it from labels keeps branch names
/// consistent with how the issue was triaged instead of forcing the operator to
/// supply it.
/// What: scans labels case-insensitively for the first recognised mapping
/// (bug → fix, enhancement/feature → feat, docs/documentation → docs,
/// refactor → refactor, chore → chore, test → test); defaults to `feat` when no
/// label matches.
/// Test: `issue_type_bug`, `issue_type_enhancement`, `issue_type_docs`,
/// `issue_type_default_feat`, `issue_type_case_insensitive`.
pub(crate) fn issue_type(labels: &[String]) -> &'static str {
    for label in labels {
        let l = label.to_lowercase();
        let mapped = match l.as_str() {
            "bug" | "fix" | "bugfix" => Some("fix"),
            "enhancement" | "feature" | "feat" => Some("feat"),
            "docs" | "documentation" => Some("docs"),
            "refactor" | "refactoring" => Some("refactor"),
            "chore" => Some("chore"),
            "test" | "tests" | "testing" => Some("test"),
            "perf" | "performance" => Some("perf"),
            _ => None,
        };
        if let Some(t) = mapped {
            return t;
        }
    }
    "feat"
}

/// Compose the full ticket branch name `<type>/<issue#>-<slug>`.
///
/// Why: this is the single source of truth for the branch a `tm ticket` run
/// targets; centralising it guarantees the branch referenced in the PR, the
/// worktree, and the commit message all agree.
/// What: combines [`issue_type`] over the labels with the issue number and
/// [`slugify`] of the title into `feat/1232-add-foo`.
/// Test: `derive_branch_name_basic`, `derive_branch_name_uses_label_type`.
pub(crate) fn derive_branch_name(issue_number: u64, title: &str, labels: &[String]) -> String {
    format!("{}/{}-{}", issue_type(labels), issue_number, slugify(title))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Add the thing"), "add-the-thing");
    }

    #[test]
    fn slugify_collapses_punctuation() {
        assert_eq!(
            slugify("feat(trusty-mpm): one-shot  workflow!!"),
            "feat-trusty-mpm-one-shot-workflow"
        );
    }

    #[test]
    fn slugify_truncates_long_title() {
        let title =
            "this is a very long issue title that should be truncated at a word boundary somewhere";
        let slug = slugify(title);
        assert!(slug.len() <= MAX_SLUG_LEN, "slug too long: {slug}");
        // Must not end on a dangling dash and must be a prefix of the *fully
        // slugified* title (truncation only drops a trailing word, never
        // rewrites earlier characters). Comparing against the slugified form —
        // rather than a naive space→dash replace — keeps this robust to any
        // punctuation in the title.
        assert!(!slug.ends_with('-'));
        let full = slugify(title);
        assert!(
            full.starts_with(&slug),
            "slug `{slug}` is not a prefix of full slug `{full}`"
        );
    }

    #[test]
    fn slugify_empty_falls_back() {
        assert_eq!(slugify(""), "issue");
        assert_eq!(slugify("!!!"), "issue");
    }

    #[test]
    fn slugify_preserves_accented_latin() {
        // Accented Latin must survive: lowercased and kept, not dashed away.
        let slug = slugify("Café déjà vu");
        assert_ne!(slug, "issue", "accented title collapsed to fallback");
        assert_eq!(slug, "café-déjà-vu");
    }

    #[test]
    fn slugify_preserves_cjk() {
        // CJK titles must produce a real slug, not the `issue` fallback.
        let slug = slugify("課題を修正する");
        assert_ne!(slug, "issue", "CJK title collapsed to fallback");
        assert_eq!(slug, "課題を修正する");
    }

    #[test]
    fn slugify_truncates_multibyte_safely() {
        // A long all-multibyte title with no dashes must truncate on a UTF-8
        // char boundary (never panic) and stay within the byte cap.
        let title = "あ".repeat(60);
        let slug = slugify(&title);
        assert!(slug.len() <= MAX_SLUG_LEN, "slug too long: {slug}");
        assert!(!slug.is_empty());
    }

    #[test]
    fn issue_type_bug() {
        assert_eq!(issue_type(&["bug".to_string()]), "fix");
    }

    #[test]
    fn issue_type_enhancement() {
        assert_eq!(issue_type(&["enhancement".to_string()]), "feat");
    }

    #[test]
    fn issue_type_docs() {
        assert_eq!(issue_type(&["documentation".to_string()]), "docs");
    }

    #[test]
    fn issue_type_default_feat() {
        assert_eq!(issue_type(&["wontfix".to_string()]), "feat");
        assert_eq!(issue_type(&[]), "feat");
    }

    #[test]
    fn issue_type_case_insensitive() {
        assert_eq!(issue_type(&["BUG".to_string()]), "fix");
        assert_eq!(issue_type(&["Refactor".to_string()]), "refactor");
    }

    #[test]
    fn issue_type_first_match_wins() {
        // First recognised label decides; an unknown label before it is skipped.
        let labels = vec!["needs-triage".to_string(), "bug".to_string()];
        assert_eq!(issue_type(&labels), "fix");
    }

    #[test]
    fn derive_branch_name_basic() {
        assert_eq!(
            derive_branch_name(1232, "Add the thing", &[]),
            "feat/1232-add-the-thing"
        );
    }

    #[test]
    fn derive_branch_name_uses_label_type() {
        assert_eq!(
            derive_branch_name(7, "Fix the crash", &["bug".to_string()]),
            "fix/7-fix-the-crash"
        );
    }
}

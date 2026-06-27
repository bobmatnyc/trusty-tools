//! Canonical project-slug derivation shared across the trusty-* workspace.
//!
//! Why: trusty-memory derives palace names from a directory basename (and from
//! arbitrary repo names supplied on the CLI), and trusty-installer's
//! `ensure` must produce the *byte-for-byte identical* slug or the trusty-memory
//! daemon rejects `POST /api/v1/palaces` with a 400 (its `validate_palace_name`
//! re-derives the slug and compares). Before #1348 each crate carried its own
//! copy of the rule, a silent-divergence hazard: a tweak in one would let the
//! two drift apart without any compile-time signal. Hoisting the single rule
//! into `trusty-common` makes it the one source of truth both crates call.
//!
//! What: [`slugify_string`] — the one canonicalisation function. trusty-memory
//! re-exports it as `trusty_memory::messaging::slugify_string` (a thin shim) and
//! trusty-installer calls it directly.
//!
//! Test: `cargo test -p trusty-common -- slug::tests` pins the canonical
//! behaviour here; the consuming crates inherit it transitively.

/// Canonicalise an arbitrary string into a stable project slug.
///
/// Why: a project's palace name / index id must be derived deterministically so
/// re-running derivation (across renames, casing differences, or
/// underscore-vs-hyphen typing) yields the same token — otherwise two callers
/// produce two different palaces for the same project, or the trusty-memory
/// daemon's `validate_palace_name` rejects creation because the controller's
/// slug disagrees with the daemon's. This is the single source of truth for that
/// rule (issue #1348).
/// What: lower-cases the trimmed input, strips a trailing `.git`, then maps each
/// character: `[a-z0-9]` pass through verbatim; `_`, `-`, space, and tab each
/// become a single `-` (runs collapse to one, never leading); every other
/// character (including `/`, `!`, and non-ASCII) is stripped entirely. Leading
/// and trailing `-` are trimmed. A pure-unicode input yields an empty string —
/// callers must guard that case.
/// Test: `slug::tests::slug_derivation_cases`.
pub fn slugify_string(input: &str) -> String {
    let lowered = input.trim().to_ascii_lowercase();
    let stripped = lowered.strip_suffix(".git").unwrap_or(&lowered);
    let mut out = String::with_capacity(stripped.len());
    let mut prev_hyphen = false;
    for c in stripped.chars() {
        let next = match c {
            'a'..='z' | '0'..='9' => Some(c),
            '_' | '-' | ' ' | '\t' => Some('-'),
            // Strip everything else.
            _ => None,
        };
        if let Some(c) = next {
            if c == '-' {
                if !prev_hyphen && !out.is_empty() {
                    out.push('-');
                    prev_hyphen = true;
                }
            } else {
                out.push(c);
                prev_hyphen = false;
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: this is the canonical behaviour both trusty-memory and
    /// trusty-installer depend on; pinning every representative case here
    /// guarantees the rule cannot silently change for either consumer.
    /// What: case folding, `_`/space/tab → `-`, hyphen-run collapse, `.git`
    /// strip, leading/trailing trim, foreign-char stripping, and the empty
    /// (pure-unicode) fallthrough.
    /// Test: This is the test.
    #[test]
    fn slug_derivation_cases() {
        // Basic lowercase + hyphenation.
        assert_eq!(slugify_string("trusty-tools"), "trusty-tools");
        assert_eq!(slugify_string("Trusty_Tools"), "trusty-tools");
        assert_eq!(slugify_string("trusty tools"), "trusty-tools");
        assert_eq!(slugify_string("  trusty   tools  "), "trusty-tools");
        // Git suffix stripped.
        assert_eq!(slugify_string("trusty-tools.git"), "trusty-tools");
        // Non-alphanumerics (other than the separator set) are stripped, not
        // mapped to a hyphen.
        assert_eq!(slugify_string("trusty/tools!"), "trustytools");
        // Multiple consecutive hyphens collapse to one.
        assert_eq!(slugify_string("foo--bar"), "foo-bar");
        // Mixed separators + leading/trailing junk (trusty-installer parity).
        assert_eq!(slugify_string("My_Cool Project"), "my-cool-project");
        assert_eq!(slugify_string("  --weird__name--  "), "weird-name");
        assert_eq!(slugify_string("!!!"), "");
        // Pure unicode -> empty (caller must guard).
        assert_eq!(slugify_string("漢字"), "");
    }
}

//! Git remote-URL canonicalization for catalog sync.
//!
//! Why: `catalog_sync` decides whether an existing checkout's `origin` matches
//! the configured repo before choosing update-in-place vs a destructive
//! re-clone. Two URLs for the same repository can differ by protocol (ssh vs
//! https), a `.git` suffix, a trailing slash, or case — without normalization
//! they falsely mismatch and trigger a spurious re-clone. Factoring the
//! canonicalization into its own module keeps `catalog_sync.rs` focused (and
//! under the SLOC cap) while making the comparison independently testable.
//! What: [`urls_match`] returns whether two remote URLs refer to the same repo by
//! comparing their [`normalize_repo_url`] canonical forms.
//! Test: `urls_match_normalises_variants` in this module.

/// Return true if two git remote URLs refer to the same repository.
///
/// Why: `git remote get-url` and the configured URL may differ by trailing
/// slash, `.git` suffix, protocol, or case; normalising before comparison avoids
/// spurious re-clones when the URL forms are equivalent.
/// What: compares the [`normalize_repo_url`] canonical forms of `a` and `b`.
/// Test: `urls_match_normalises_variants`.
pub fn urls_match(a: &str, b: &str) -> bool {
    normalize_repo_url(a) == normalize_repo_url(b)
}

/// Normalise a git remote URL to a canonical form for comparison.
///
/// Why: two URLs for the same repository can differ in protocol (ssh vs https),
/// `.git` suffix, trailing slash, and case — without normalization they falsely
/// mismatch and trigger a spurious destructive re-clone.
/// What: converts `git@host:owner/repo` to `https://host/owner/repo`, strips
/// trailing `/` and `.git`, then lowercases the result.
/// Test: `urls_match_normalises_variants`.
fn normalize_repo_url(url: &str) -> String {
    // Convert SSH git@ form to HTTPS before further normalization.
    let https_form = if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            format!("https://{host}/{path}")
        } else {
            url.to_owned()
        }
    } else {
        url.to_owned()
    };
    https_form
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_match_normalises_variants() {
        // SSH and HTTPS forms of the same repo must match.
        assert!(
            urls_match(
                "git@github.com:owner/repo.git",
                "https://github.com/owner/repo"
            ),
            "ssh↔https must match"
        );
        // Trailing slash must not prevent a match.
        assert!(
            urls_match(
                "https://github.com/owner/repo/",
                "https://github.com/owner/repo"
            ),
            "trailing slash must be ignored"
        );
        // .git suffix must not prevent a match.
        assert!(
            urls_match(
                "https://github.com/owner/repo.git",
                "https://github.com/owner/repo"
            ),
            ".git suffix must be stripped"
        );
        // Case insensitivity.
        assert!(
            urls_match(
                "HTTPS://GitHub.com/Owner/Repo",
                "https://github.com/owner/repo"
            ),
            "comparison must be case-insensitive"
        );
        // Different repos must NOT match.
        assert!(
            !urls_match(
                "https://github.com/owner/repo-a",
                "https://github.com/owner/repo-b"
            ),
            "distinct repos must not match"
        );
    }
}

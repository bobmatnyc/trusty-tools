//! Tests for [`super::redact_url`] (#9124).
//!
//! Why: the helper is the only thing standing between an operator's
//! token-bearing `remote.origin.url` and the daemon log.
//! Test: this IS the test module.

use super::*;

/// A synthetic token; no assertion here ever prints the redacted text.
const TOKEN: &str = "ghp_9124SyntheticNotARealToken0000";

/// Test: this test.
#[test]
fn redact_url_strips_user_and_token() {
    let url = format!("https://octo:{TOKEN}@github.com/acme/widget.git");
    let out = redact_url(&url);
    assert!(!out.contains(TOKEN), "the token survived");
    assert!(!out.contains("octo"), "the user survived");
    assert_eq!(out, "https://***@github.com/acme/widget.git");
}

/// Test: this test.
#[test]
fn redact_url_strips_an_x_access_token() {
    let url = format!("https://x-access-token:{TOKEN}@github.com/acme/widget");
    assert_eq!(redact_url(&url), "https://***@github.com/acme/widget");
    // A bare token as the user, with no password.
    let bare = format!("https://{TOKEN}@github.com/acme/widget");
    assert_eq!(redact_url(&bare), "https://***@github.com/acme/widget");
}

/// Why: an `@` after the authority is path or query text, not userinfo, and a
/// URL with no userinfo must come back untouched and unallocated.
/// Test: this test.
#[test]
fn redact_url_leaves_a_clean_url_alone() {
    for clean in [
        "https://github.com/acme/widget.git",
        "git@github.com:acme/widget.git",
        "https://github.com/acme/widget/blob/main/a@b.md",
        "/Users/someone/projects/widget",
    ] {
        assert!(
            matches!(redact_url(clean), Cow::Borrowed(s) if s == clean),
            "{clean} was rewritten"
        );
    }
}

/// Why: git stderr and `anyhow` chains quote the URL inside prose.
/// Test: this test.
#[test]
fn redact_url_redacts_every_url_in_free_text() {
    let text = format!(
        "fatal: unable to access 'https://octo:{TOKEN}@github.com/a/b.git/': refused; \
         also ssh://deploy:{TOKEN}@host:22/c.git"
    );
    let out = redact_url(&text);
    assert!(!out.contains(TOKEN), "a token survived in free text");
    assert!(out.contains("'https://***@github.com/a/b.git/'"));
    assert!(out.contains("ssh://***@host:22/c.git"));
}

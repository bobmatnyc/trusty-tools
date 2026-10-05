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

/// Why: git accepts a raw `/` in a password; the authority then ends at that
/// `/`, and a redaction keyed on the authority alone printed the tail.
/// Test: this test.
#[test]
fn redact_url_masks_a_password_holding_a_raw_slash() {
    let url = "https://octo:ghp_9124Head/Tail9124Secret@github.com/acme/widget.git";
    let out = redact_url(url);
    assert!(!out.contains("Tail9124Secret"), "the tail survived");
    assert!(!out.contains("ghp_9124Head"), "the head survived");
    assert_eq!(out, "https://***@github.com/acme/widget.git");
}

/// Why (#9124 delta critic): a raw `?` or `#` in a password ends the authority
/// too, and the old search stopped at the query or fragment, so the password
/// survived whole.
/// Test: this test.
#[test]
fn redact_url_masks_a_password_holding_a_raw_query_or_fragment_char() {
    for url in [
        "https://octo:ab#cd9124Secret@github.com/x",
        "https://octo:ab?cd9124Secret@github.com/x",
        "https://octo:a?b#c@d9124Secret@github.com/x?page=2#top",
    ] {
        let out = redact_url(url);
        assert!(!out.contains("9124Secret"), "a password survived");
        assert!(!out.contains("octo"), "the user survived");
        assert!(out.starts_with("https://***@github.com/x"), "{out}");
    }
    // An `@` in a later query is over-redacted: the safe direction.
    let query = "https://host:8443/p?who=a@b";
    assert_eq!(redact_url(query), "https://***@b");
}

/// Why: GitLab's `private_token`, OAuth's `access_token` and friends carry the
/// credential in the query string, not the userinfo.
/// Test: this test.
#[test]
fn redact_url_masks_query_string_tokens() {
    for key in [
        "access_token",
        "private_token",
        "token",
        "oauth_token",
        "Access_Token",
    ] {
        let url =
            format!("https://gitlab.example/api/v4/projects?per_page=5&{key}={TOKEN}&x=1#top");
        let out = redact_url(&url);
        assert!(!out.contains(TOKEN), "{key}: the token survived");
        assert_eq!(
            out,
            format!("https://gitlab.example/api/v4/projects?per_page=5&{key}=***&x=1#top")
        );
    }
    let first = format!("fetch 'https://h/o/r.git?token={TOKEN}' failed");
    assert_eq!(
        redact_url(&first),
        "fetch 'https://h/o/r.git?token=***' failed"
    );
    // A key that only ends in `token` is not one of the listed keys.
    let other = "https://h/o?mytoken=abc&page=2";
    assert!(matches!(redact_url(other), Cow::Borrowed(_)));
}

/// Why (#9227): a stored URL is one URL, not free text; a quote or space in
/// its password ended `redact_url`'s authority early and the password printed.
/// Test: this test.
#[test]
fn redact_stored_url_masks_a_quoted_or_spaced_password() {
    for (i, (url, want)) in [
        ("https://u:pa'ss9227@host/o/r", "https://***@host/o/r"),
        ("https://u:pa ss9227@host/o/r", "https://***@host/o/r"),
        ("https://u:pa\"ss9227@host/o/r", "https://***@host/o/r"),
        ("https://u:pa`ss9227@host/o/r", "https://***@host/o/r"),
        ("https://u:pa\tss9227@host/o/r", "https://***@host/o/r"),
        ("ssh://u:pa'ss9227@host:22/o/r", "ssh://***@host:22/o/r"),
        ("https://u:pa/ss9227@host/o/r", "https://***@host/o/r"),
        ("https://u:p'a/ss9227@host/o/r", "https://***@host/o/r"),
        (
            "https://u:pa'ss9227@host/o/r?token=ss9227",
            "https://***@host/o/r?token=***",
        ),
        ("u:pa'ss9227@host:o/r", "***@host:o/r"),
        ("u:pa ss9227@h:o/r://x", "***@h:o/r://x"),
    ]
    .into_iter()
    .enumerate()
    {
        let out = redact_stored_url(url);
        assert!(
            !out.contains("ss9227"),
            "case {i}: a password fragment survived"
        );
        assert!(
            !out.contains("u:pa"),
            "case {i}: the user:password pair survived"
        );
        assert!(out == want, "case {i}: unexpected masked form");
    }
}

/// Why (#9227): `tm list` and the duplicate-alias error must show a clean
/// URL exactly as they did before the stored-URL boundary.
/// Test: this test.
#[test]
fn redact_stored_url_keeps_a_clean_url_as_redact_url_shows_it() {
    for (url, want) in [
        ("https://github.com/o/r.git", "https://github.com/o/r.git"),
        ("git@github.com:o/r.git", "git@github.com:o/r.git"),
        ("ssh://git@host:22/o/r.git", "ssh://***@host:22/o/r.git"),
        (
            "https://github.com/o/r/blob/main/a@b.md",
            "https://github.com/o/r/blob/main/a@b.md",
        ),
        ("/srv/dir@x/repo", "/srv/dir@x/repo"),
    ] {
        assert_eq!(redact_stored_url(url), want);
        assert_eq!(redact_stored_url(url), redact_url(url), "{url}");
    }
    assert!(matches!(
        redact_stored_url("https://github.com/o/r.git"),
        Cow::Borrowed(_)
    ));
}

//! [`cap_detail`]: one bounded, redacted line (#9194).
//!
//! Why: a detail carries error text from a transport or a third-party API,
//! which can be long, multi-line, or hold a credential (R3).
//! What: drives `cap_detail` directly.
//! Test: this module.

use super::*;

/// #9194 E6: whitespace runs collapse to one space; the result is at most
/// 200 characters, cut on a character boundary with a visible marker.
#[test]
fn cap_detail_cuts_to_one_line_of_200_characters() {
    let long = format!("héllo\n\tworld  {}", "é".repeat(400));
    let detail = cap_detail(&long);
    assert_eq!(detail.chars().count(), 200);
    assert!(detail.starts_with("héllo world é"), "{detail}");
    assert!(detail.ends_with('…'), "{detail}");
    assert_eq!(cap_detail("  short\r\n text "), "short text");
}

/// #9194 R3: a bearer token, a `token=` value, a basic credential and a
/// long credential-shaped run never survive into a detail.
#[test]
fn cap_detail_redacts_credentials() {
    for (text, secret) in [
        ("Authorization: Bearer abc.def", "abc.def"),
        ("authorization: basic dXNlcjpwYXNz", "dXNlcjpwYXNz"),
        ("GET /x?access_token=t0ps3cret&a=1", "t0ps3cret"),
        ("password: hunter2", "hunter2"),
        ("key ghp_0123456789abcdefABCDEF0123456789", "ghp_0123456789"),
    ] {
        let detail = cap_detail(text);
        assert!(!detail.contains(secret), "{secret} survived: {detail}");
        assert!(
            detail.contains("[redacted]") || detail.contains("[masked"),
            "{detail}"
        );
    }
    assert_eq!(cap_detail("no hits for the query"), "no hits for the query");
}

/// #9194: `Authorization:Bearer <token>` with no space after the colon still
/// hides the token.
#[test]
fn cap_detail_masks_bearer_without_a_space() {
    let detail = cap_detail("401 with Authorization:Bearer abc.def sent");
    assert!(!detail.contains("abc.def"), "token survived: {detail}");
    assert_eq!(detail, "401 with Authorization:Bearer [redacted] sent");
}

/// #9194: the value of an `X-Api-Key:` header is hidden, even a short one
/// with no digit that the credential-shape mask would keep.
#[test]
fn cap_detail_masks_an_x_api_key_value() {
    let detail = cap_detail("403 with X-Api-Key: shortkey sent");
    assert!(!detail.contains("shortkey"), "key survived: {detail}");
    assert_eq!(detail, "403 with X-Api-Key: [redacted] sent");
}

/// #9194: the password in a URL's userinfo is hidden; the user, host and
/// path stay readable.
#[test]
fn cap_detail_masks_a_url_userinfo_password() {
    let detail = cap_detail("connect to http://user:PASS1234@127.0.0.1:9/p failed");
    assert!(!detail.contains("PASS1234"), "password survived: {detail}");
    assert_eq!(
        detail,
        "connect to http://user:[redacted]@127.0.0.1:9/p failed"
    );
}

/// #9431: an error message naming a credentialed URL keeps its whitespace,
/// host, path and wording; only the password and the token value change.
#[test]
fn redact_credentials_keeps_the_message_and_hides_url_credentials() {
    let text = "GET http://user:fake123fake@127.0.0.1:9/p?access_token=fake123fake/health:\n  \
                error sending request for url (http://127.0.0.1:9/p?access_token=fake123fake/health)";
    let shown = redact_credentials(text);
    assert_eq!(
        shown,
        "GET http://user:[redacted]@127.0.0.1:9/p?access_token=[redacted]\n  error sending \
         request for url (http://127.0.0.1:9/p?access_token=[redacted]"
    );
    // #9431: a username-only userinfo is sent as Basic auth, so all of it goes.
    assert_eq!(
        redact_credentials("GET http://tok123@127.0.0.1:9/p: refused"),
        "GET http://[redacted]@127.0.0.1:9/p: refused"
    );
    let plain = "index `trusty-tools-4e2cf878` not found\n\tat 10.0.0.1";
    assert_eq!(
        redact_credentials(plain),
        plain,
        "nothing to hide, nothing changed"
    );
}

/// #9194: a header value glued to its colon (`X-Api-Key:shortkey`) is
/// hidden, as are `Token:` and `Password:` values written the same way.
#[test]
fn cap_detail_masks_a_header_value_glued_to_its_colon() {
    for (text, secret, want) in [
        (
            "X-Api-Key:shortkey sent",
            "shortkey",
            "X-Api-Key:[redacted] sent",
        ),
        ("Token:tokvalue sent", "tokvalue", "Token:[redacted] sent"),
        (
            "Password:hunter2 sent",
            "hunter2",
            "Password:[redacted] sent",
        ),
    ] {
        let detail = cap_detail(text);
        assert!(!detail.contains(secret), "{secret} survived: {detail}");
        assert_eq!(detail, want);
    }
}

/// #9194: quotes around a JSON header name and scheme do not stop the token
/// after `"Authorization":"Bearer` from being hidden.
#[test]
fn cap_detail_masks_a_json_quoted_authorization_header() {
    let detail = cap_detail(r#"401 with {"Authorization":"Bearer abc123"} sent"#);
    assert!(!detail.contains("abc123"), "token survived: {detail}");
    assert_eq!(
        detail,
        r#"401 with {"Authorization":"Bearer [redacted] sent"#
    );
}

/// #9194: `authorization=bearer <token>` hides the token in the next word,
/// not only the `bearer` value of the pair.
#[test]
fn cap_detail_masks_an_equals_joined_bearer_scheme() {
    let detail = cap_detail("query authorization=bearer abc123 rejected");
    assert!(!detail.contains("abc123"), "token survived: {detail}");
    assert_eq!(detail, "query authorization=bearer [redacted] rejected");
}

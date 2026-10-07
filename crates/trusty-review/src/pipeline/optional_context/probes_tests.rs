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
